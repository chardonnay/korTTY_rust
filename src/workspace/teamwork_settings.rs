//! Owned editor for Teamwork connection sources.

use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Size, Task, alignment, window};

use crate::connection_store::teamwork::{
    DEFAULT_CHECK_INTERVAL_MINUTES, MAX_CHECK_INTERVAL_MINUTES, MAX_LOCATION_BYTES, MAX_SOURCES,
    ManagedSource, Settings, Source, SourceKind,
};

use super::backup_data::teamwork::{Proposals, SourceProposal};
use super::dialog_placement::{Kind, escape};
use super::{Message as Desktop, Workspace};

const TITLE: &str = "Teamwork Settings";
const INTERVAL_STEP: u16 = 5;

#[derive(Clone, Debug)]
pub enum Message {
    Open,
    Focus,
    Close,
    Retry,
    Loaded(Result<Settings, String>),
    Add,
    AddJavaSource(usize),
    UseJavaInterval,
    DefaultInterval(String),
    SourceType(usize, SourceKind),
    Location(usize, String),
    Interval(usize, String),
    Enable(usize, bool),
    Remove(usize),
    Save,
    Saved(Result<(), String>),
    Refresh,
    KeepEditing,
    Discard,
}

#[derive(Default)]
pub(super) struct State(Option<Dialog>);

struct Dialog {
    id: window::Id,
    owner: window::Id,
    phase: Phase,
}

enum Phase {
    Managed,
    Loading,
    Unavailable(String),
    Editing(Draft),
    ConfirmDiscard(Draft),
    Saving(Draft, Settings),
}

#[derive(Clone)]
struct Draft {
    before: Settings,
    default_interval: String,
    sources: Vec<SourceDraft>,
    problem: Option<String>,
}

#[derive(Clone)]
struct SourceDraft {
    kind: SourceKind,
    location: String,
    enabled: bool,
    interval: String,
}

impl Draft {
    fn can_add_java_source(&self, source: &SourceProposal) -> bool {
        if self.sources.len() >= MAX_SOURCES {
            return false;
        }
        let origin = Source {
            kind: source.kind,
            location: source.location.clone(),
            enabled: false,
            check_interval_minutes: source.interval,
        }
        .origin()
        .ok();
        !self.sources.iter().any(|saved| {
            saved.kind == source.kind && saved.location.trim() == source.location
                || origin.as_ref().is_some_and(|origin| {
                    (Source {
                        kind: saved.kind,
                        location: saved.location.clone(),
                        enabled: false,
                        check_interval_minutes: source.interval,
                    })
                    .origin()
                    .as_ref()
                    .is_ok_and(|saved| saved == origin)
                })
        })
    }

    fn add_java_source(&mut self, source: &SourceProposal) {
        if self.can_add_java_source(source) {
            self.sources.push(SourceDraft {
                kind: source.kind,
                location: source.location.clone(),
                enabled: false,
                interval: source.interval.to_string(),
            });
        }
    }

    fn new(settings: Settings) -> Self {
        let default_interval = settings.default_check_interval_minutes.to_string();
        let sources = settings
            .sources
            .iter()
            .map(|source| SourceDraft {
                kind: source.kind,
                location: source.location.clone(),
                enabled: source.enabled,
                interval: source.check_interval_minutes.to_string(),
            })
            .collect();
        Self {
            before: settings,
            default_interval,
            sources,
            problem: None,
        }
    }

    fn dirty(&self) -> bool {
        minutes(&self.default_interval) != Some(self.before.default_check_interval_minutes)
            || self.sources.len() != self.before.sources.len()
            || self
                .sources
                .iter()
                .zip(&self.before.sources)
                .any(|(draft, saved)| {
                    draft.kind != saved.kind
                        || draft.location != saved.location
                        || draft.enabled != saved.enabled
                        || minutes(&draft.interval) != Some(saved.check_interval_minutes)
                })
    }

    fn candidate(&self) -> Result<Settings, String> {
        let mut candidate = self.before.clone();
        candidate.default_check_interval_minutes = minutes(&self.default_interval)
            .ok_or("Enter a default check interval between 1 and 1440 minutes.")?;
        candidate.sources = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                let check_interval_minutes = minutes(&draft.interval).ok_or_else(|| {
                    format!(
                        "Enter a check interval between 1 and 1440 minutes for source {}.",
                        index + 1
                    )
                })?;
                Ok(Source {
                    kind: draft.kind,
                    location: draft.location.clone(),
                    enabled: draft.enabled,
                    check_interval_minutes,
                })
            })
            .collect::<Result<_, String>>()?;
        candidate.validate().map_err(|error| error.to_string())?;
        Ok(candidate)
    }
}

fn minutes(input: &str) -> Option<u16> {
    if !interval_text(input) {
        return None;
    }
    input
        .parse()
        .ok()
        .filter(|value| (1..=MAX_CHECK_INTERVAL_MINUTES).contains(value))
}

fn interval_text(input: &str) -> bool {
    input.len() <= 4 && input.bytes().all(|byte| byte.is_ascii_digit())
}

impl Dialog {
    fn edit(&mut self) -> Option<&mut Draft> {
        let Phase::Editing(draft) = &mut self.phase else {
            return None;
        };
        draft.problem = None;
        Some(draft)
    }

    fn save(&mut self) -> Task<Desktop> {
        let Phase::Editing(draft) = &self.phase else {
            return Task::none();
        };
        let value = match draft.candidate() {
            Ok(value) => value,
            Err(error) => {
                if let Some(draft) = self.edit() {
                    draft.problem = Some(error);
                }
                return Task::none();
            }
        };
        let before = draft.before.clone();
        self.phase = Phase::Saving(draft.clone(), value.clone());
        let id = self.id;
        Task::perform(
            async move { value.save(&before).map_err(|error| error.to_string()) },
            move |result| Desktop::TeamworkSettings(id, Message::Saved(result)),
        )
    }
}

impl Workspace {
    pub(super) fn update_teamwork_settings(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Open => return self.open_teamwork_settings(id),
            Message::Close => return self.close_teamwork_settings(id).unwrap_or_else(Task::none),
            _ => {}
        }
        let allowed = self.teamwork_sync_allowed();
        let custom = self.custom_teamwork_sources_allowed();
        let java = self.window_settings.java_teamwork.as_ref();
        let Some(dialog) = self
            .teamwork_settings
            .0
            .as_mut()
            .filter(|dialog| dialog.id == id)
        else {
            return Task::none();
        };
        match message {
            Message::Focus => return window::gain_focus(id),
            Message::Refresh if allowed && !custom && matches!(dialog.phase, Phase::Managed) => {
                return Task::batch([
                    self.finish_teamwork_settings(),
                    self.teamwork_settings_saved(&Settings::default()),
                ]);
            }
            Message::Retry if custom && matches!(dialog.phase, Phase::Unavailable(_)) => {
                dialog.phase = Phase::Loading;
                return load(id);
            }
            Message::Loaded(result) if custom && matches!(dialog.phase, Phase::Loading) => {
                dialog.phase = match result {
                    Ok(settings) => Phase::Editing(Draft::new(settings)),
                    Err(error) => Phase::Unavailable(error),
                };
            }
            Message::Add if custom => {
                if let Some(draft) = dialog
                    .edit()
                    .filter(|draft| draft.sources.len() < MAX_SOURCES)
                {
                    draft.sources.push(SourceDraft {
                        kind: SourceKind::File,
                        location: String::new(),
                        enabled: true,
                        interval: DEFAULT_CHECK_INTERVAL_MINUTES.to_string(),
                    });
                }
            }
            Message::AddJavaSource(index) if custom => {
                if let Some(source) = java.and_then(|proposals| proposals.sources.get(index))
                    && let Some(draft) = dialog.edit()
                {
                    draft.add_java_source(source);
                }
            }
            Message::UseJavaInterval if custom => {
                if let Some(interval) = java.and_then(|proposals| proposals.interval)
                    && let Some(draft) = dialog.edit()
                {
                    draft.default_interval = interval.to_string();
                }
            }
            Message::DefaultInterval(value) if custom && interval_text(&value) => {
                if let Some(draft) = dialog.edit() {
                    draft.default_interval = value;
                }
            }
            Message::SourceType(index, kind) if custom => {
                if let Some(source) = dialog.edit().and_then(|draft| draft.sources.get_mut(index))
                    && source.kind != kind
                {
                    source.kind = kind;
                    source.location.clear();
                }
            }
            Message::Location(index, value) if custom && value.len() <= MAX_LOCATION_BYTES => {
                if let Some(source) = dialog.edit().and_then(|draft| draft.sources.get_mut(index)) {
                    source.location = value;
                }
            }
            Message::Interval(index, value) if custom && interval_text(&value) => {
                if let Some(source) = dialog.edit().and_then(|draft| draft.sources.get_mut(index)) {
                    source.interval = value;
                }
            }
            Message::Enable(index, enabled) if custom => {
                if let Some(source) = dialog.edit().and_then(|draft| draft.sources.get_mut(index)) {
                    source.enabled = enabled;
                }
            }
            Message::Remove(index) if custom => {
                if let Some(draft) = dialog.edit().filter(|draft| index < draft.sources.len()) {
                    draft.sources.remove(index);
                }
            }
            Message::Save if custom => return dialog.save(),
            Message::Saved(result) => {
                if let Phase::Saving(draft, saved_settings) = &mut dialog.phase {
                    match result {
                        Ok(()) => {
                            let saved_settings = saved_settings.clone();
                            let close = self.finish_teamwork_settings();
                            return Task::batch([
                                close,
                                self.teamwork_settings_saved(&saved_settings),
                            ]);
                        }
                        Err(error) => {
                            draft.problem =
                                Some(format!("Could not save Teamwork settings ({error})."));
                            dialog.phase = Phase::Editing(draft.clone());
                        }
                    }
                }
            }
            Message::KeepEditing => {
                if let Phase::ConfirmDiscard(draft) = &dialog.phase {
                    dialog.phase = Phase::Editing(draft.clone());
                }
            }
            Message::Discard if matches!(dialog.phase, Phase::ConfirmDiscard(_)) => {
                return self.finish_teamwork_settings();
            }
            _ => {}
        }
        Task::none()
    }

    fn open_teamwork_settings(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || !self.windows.contains_key(&owner) || !self.teamwork_sync_allowed() {
            return Task::none();
        }
        if let Some(dialog) = &self.teamwork_settings.0 {
            return window::gain_focus(dialog.id);
        }
        if let Some((dialog, _)) = self.owner_dialog(owner) {
            return window::gain_focus(dialog);
        }
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            Kind::TeamworkSettings,
            window::Settings {
                size: Size::new(730.0, 430.0),
                min_size: Some(Size::new(440.0, 245.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(focus),
        );
        let custom = self.custom_teamwork_sources_allowed();
        self.teamwork_settings.0 = Some(Dialog {
            id,
            owner,
            phase: if custom {
                Phase::Loading
            } else {
                Phase::Managed
            },
        });
        if custom {
            Task::batch([opened, load(id)])
        } else {
            opened
        }
    }

    pub(super) fn teamwork_settings_title(&self, id: window::Id) -> Option<&'static str> {
        self.teamwork_settings
            .0
            .as_ref()
            .filter(|dialog| dialog.id == id)
            .map(|_| TITLE)
    }

    pub(super) fn teamwork_settings_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.teamwork_settings
            .0
            .as_ref()
            .filter(|dialog| dialog.owner == owner)
            .map(|dialog| dialog.id)
    }

    pub(super) fn close_teamwork_settings(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self
            .teamwork_settings
            .0
            .as_mut()
            .filter(|dialog| dialog.id == id)?;
        match &dialog.phase {
            Phase::Saving(..) | Phase::ConfirmDiscard(_) => Some(Task::none()),
            Phase::Editing(draft) if draft.dirty() => {
                dialog.phase = Phase::ConfirmDiscard(draft.clone());
                Some(Task::none())
            }
            _ => Some(self.finish_teamwork_settings()),
        }
    }

    pub(super) fn teamwork_settings_closed(&mut self, id: window::Id) {
        if self.teamwork_settings_title(id).is_some() {
            self.teamwork_settings.0 = None;
        }
    }

    pub(super) fn guard_teamwork_settings_quit(&mut self) -> Option<Task<Desktop>> {
        let dialog = self.teamwork_settings.0.as_ref()?;
        let pending = match &dialog.phase {
            Phase::Editing(draft) => draft.dirty(),
            Phase::Saving(..) | Phase::ConfirmDiscard(_) => true,
            _ => false,
        };
        if !pending {
            return None;
        }
        let id = dialog.id;
        let close = self.close_teamwork_settings(id)?;
        Some(Task::batch([close, window::gain_focus(id)]))
    }

    fn finish_teamwork_settings(&mut self) -> Task<Desktop> {
        let Some(Dialog { id, owner, .. }) = self.teamwork_settings.0.take() else {
            return Task::none();
        };
        self.close_placed_dialog_and_focus(id, owner)
    }

    pub(super) fn teamwork_settings_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self
            .teamwork_settings
            .0
            .as_ref()
            .filter(|dialog| dialog.id == id)?;
        let allowed = self.teamwork_sync_allowed();
        let custom = self.custom_teamwork_sources_allowed();
        let managed = &self.clipboard.policy.teamwork_sources;
        let (mut body, footer) = match &dialog.phase {
            _ if !allowed => (
                column![
                    text("Teamwork is unavailable under the current policy.")
                        .wrapping(Wrapping::WordOrGlyph)
                ],
                row![button("Close").on_press(Message::Close)],
            ),
            Phase::Managed => (
                column![
                    text("Your organization allows managed Teamwork sources only.")
                        .wrapping(Wrapping::WordOrGlyph)
                ]
                .spacing(12),
                row![
                    button("Close").on_press(Message::Close),
                    button("Refresh now").on_press_maybe((!custom).then_some(Message::Refresh)),
                ]
                .spacing(8),
            ),
            Phase::Loading => (
                column![text("Loading shared connection sources…")],
                footer(false),
            ),
            Phase::Unavailable(error) => (
                column![
                    text("Could not load Teamwork settings.").size(17),
                    text(error).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(8),
                row![
                    button("Retry").on_press_maybe(custom.then_some(Message::Retry)),
                    button("Close").on_press(Message::Close),
                ]
                .spacing(8),
            ),
            Phase::Editing(draft) => {
                let validation = draft.candidate();
                let valid = validation.is_ok() && custom;
                let mut body = sources(draft, custom);
                if custom && let Some(java) = &self.window_settings.java_teamwork {
                    body = body.push(java_proposals(draft, java));
                }
                if !custom {
                    body = body.push(
                        text("Custom Teamwork sources are unavailable under the current policy.")
                            .size(13)
                            .wrapping(Wrapping::WordOrGlyph),
                    );
                }
                if let Some(error) = draft.problem.clone().or_else(|| validation.err()) {
                    body = body.push(text(error).size(13).wrapping(Wrapping::WordOrGlyph));
                }
                (body, footer(valid))
            }
            Phase::ConfirmDiscard(_) => (
                column![
                    text("Discard changes to Teamwork sources?").size(17),
                    text("Your saved sources will be kept.").size(14),
                ]
                .spacing(8),
                row![
                    button("Keep editing").on_press(Message::KeepEditing),
                    button("Discard")
                        .style(button::danger)
                        .on_press(Message::Discard),
                ]
                .spacing(8),
            ),
            Phase::Saving(draft, _) => (
                sources(draft, false).push(text("Saving Teamwork settings…").size(13)),
                row![button("Cancel"), button("Save")].spacing(8),
            ),
        };
        if allowed
            && !matches!(dialog.phase, Phase::ConfirmDiscard(_))
            && (!managed.is_empty() || matches!(dialog.phase, Phase::Managed))
        {
            body = body.push(managed_sources(managed));
        }
        let content: Element<'_, Message> = container(
            column![
                text("Teamwork sources").size(21),
                scrollable(body.width(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill),
                container(
                    footer
                        .align_y(Alignment::Center)
                        .wrap()
                        .align_x(alignment::Horizontal::Right),
                )
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right),
            ]
            .spacing(12)
            .height(Length::Fill),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
        let escape_action = if matches!(dialog.phase, Phase::ConfirmDiscard(_)) {
            Message::KeepEditing
        } else {
            Message::Close
        };
        Some(
            escape::on_escape(content, escape_action)
                .map(move |message| Desktop::TeamworkSettings(id, message)),
        )
    }
}

fn sources(draft: &Draft, editable: bool) -> Column<'_, Message> {
    let can_add = editable && draft.sources.len() < MAX_SOURCES;
    let mut body = column![
        text("Add a shared file or a Git repository. Connections from enabled sources appear in the Connection Manager.").size(14).wrapping(Wrapping::WordOrGlyph),
        interval_input("Default check interval", &draft.default_interval, editable, Message::DefaultInterval),
        text("Save checks enabled sources now. Interval changes take effect the next time kortty starts.").size(13).wrapping(Wrapping::WordOrGlyph),
        button("Add source").on_press_maybe(can_add.then_some(Message::Add)),
    ].spacing(10);
    if draft.sources.is_empty() {
        return body.push(text("No custom sources configured.").size(14));
    }
    for (index, source) in draft.sources.iter().enumerate() {
        let kind: Element<'_, Message> = if editable {
            pick_list(
                [SourceKind::File, SourceKind::Git],
                Some(source.kind),
                move |kind| Message::SourceType(index, kind),
            )
            .into()
        } else {
            text(source.kind.to_string()).size(14).into()
        };
        let placeholder = match source.kind {
            SourceKind::File => "Absolute path or file URL",
            SourceKind::Git => "Git repository URL",
        };
        let mut location = text_input(placeholder, &source.location).width(Length::Fill);
        if editable {
            location = location.on_input(move |value| Message::Location(index, value));
        }
        let mut enabled = checkbox(source.enabled).label("Enabled");
        if editable {
            enabled = enabled.on_toggle(move |value| Message::Enable(index, value));
        }
        body = body.push(
            container(
                column![
                    row![text(format!("Source {}", index + 1)).size(14), kind]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    location,
                    interval_input("Check interval", &source.interval, editable, move |value| {
                        Message::Interval(index, value)
                    }),
                    row![
                        enabled,
                        button("Remove").on_press_maybe(editable.then_some(Message::Remove(index))),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .wrap(),
                ]
                .spacing(7),
            )
            .padding(8)
            .width(Length::Fill)
            .style(container::bordered_box),
        );
    }
    body
}

fn managed_sources(sources: &[ManagedSource]) -> Column<'_, Message> {
    let mut body = column![text("Managed sources").size(17)].spacing(8);
    if sources.is_empty() {
        return body.push(text("No managed sources configured.").size(14));
    }
    for managed in sources {
        let source = &managed.source;
        body = body.push(
            container(
                column![
                    text(managed.name.as_str())
                        .size(15)
                        .wrapping(Wrapping::WordOrGlyph),
                    text(format!(
                        "{} · Check every {} min",
                        source.kind, source.check_interval_minutes
                    ))
                    .size(13),
                    text(source.location.as_str())
                        .size(13)
                        .wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(4),
            )
            .padding(8)
            .width(Length::Fill)
            .style(container::bordered_box),
        );
    }
    body
}

fn interval_input<'a>(
    label: &'a str,
    value: &'a str,
    editable: bool,
    change: impl Fn(String) -> Message + Copy + 'a,
) -> Column<'a, Message> {
    let interval = minutes(value);
    let mut input = text_input("1–1440", value).width(96);
    if editable {
        input = input.on_input(change);
    }
    let decrease = interval
        .filter(|&value| editable && value > 1)
        .map(|value| change(value.saturating_sub(INTERVAL_STEP).max(1).to_string()));
    let increase = interval
        .filter(|&value| editable && value < MAX_CHECK_INTERVAL_MINUTES)
        .map(|value| {
            change(
                (value + INTERVAL_STEP)
                    .min(MAX_CHECK_INTERVAL_MINUTES)
                    .to_string(),
            )
        });
    column![
        text(label).size(14),
        row![
            button("−").on_press_maybe(decrease),
            input,
            button("+").on_press_maybe(increase),
            text("minutes").size(13),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .wrap(),
        (editable && interval.is_none()).then(|| text("Enter 1–1440 minutes.").size(12)),
    ]
    .spacing(4)
}

fn footer(can_save: bool) -> Row<'static, Message> {
    row![
        button("Cancel").on_press(Message::Close),
        button("Save").on_press_maybe(can_save.then_some(Message::Save)),
    ]
    .spacing(8)
}

fn focus(id: window::Id) -> Desktop {
    Desktop::TeamworkSettings(id, Message::Focus)
}

fn java_proposals<'a>(draft: &Draft, proposals: &'a Proposals) -> Column<'a, Message> {
    let mut body = column![text("Imported Java Teamwork settings").size(17)].spacing(6);
    for (index, source) in proposals.sources.iter().enumerate() {
        let kind = match source.kind {
            SourceKind::File => "Shared file",
            SourceKind::Git => "Git repository",
        };
        let status = if source.was_enabled {
            "previously enabled"
        } else {
            "previously disabled"
        };
        body = body
            .push(
                text(format!("{kind} · {} · {status}", source.location))
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .push(
                button("Add disabled source").on_press_maybe(
                    draft
                        .can_add_java_source(source)
                        .then_some(Message::AddJavaSource(index)),
                ),
            );
    }
    if let Some(interval) = proposals.interval {
        body = body.push(
            button(text(format!(
                "Use Java default interval ({interval} minutes)"
            )))
            .on_press_maybe(
                (minutes(&draft.default_interval) != Some(interval))
                    .then_some(Message::UseJavaInterval),
            ),
        );
    }
    for (label, value) in [
        ("Credential ID", &proposals.credential),
        ("SSH key ID", &proposals.key),
        ("Username", &proposals.username),
    ] {
        if let Some(value) = value {
            body = body.push(text(format!("{label} · {value}")).wrapping(Wrapping::WordOrGlyph));
        }
    }
    if let Some(value) = proposals.temporary_key {
        body = body.push(text(if value {
            "Temporary key preference · On"
        } else {
            "Temporary key preference · Off"
        }));
    }
    if proposals.credential.is_some()
        || proposals.key.is_some()
        || proposals.username.is_some()
        || proposals.temporary_key.is_some()
    {
        body = body.push(
            text("Review these references and select local authentication in Connection Manager.")
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    body
}

fn load(id: window::Id) -> Task<Desktop> {
    Task::perform(
        async { Settings::load().map_err(|error| error.to_string()) },
        move |result| Desktop::TeamworkSettings(id, Message::Loaded(result)),
    )
}

#[cfg(test)]
mod tests {
    use iced::advanced::{Layout, layout, widget::Tree};
    use iced::{Font, Renderer};

    use super::*;
    use crate::workspace::chrome;

    #[test]
    fn java_teamwork_restore_only_adds_reviewed_disabled_sources_to_the_draft() {
        let import = super::super::backup_data::portable_java_preferences(
            br#"<globalSettings>
              <teamworkSources>
                <source><id>java-source</id><type>GIT</type><location>https://git.example/shared</location><checkIntervalMinutes>20</checkIntervalMinutes><enabled>true</enabled></source>
                <source><id>java-source</id><location>https://git.example/duplicate</location></source>
                <source><location>https://user:password@git.example/secret</location></source>
                <source><type>SHARED_FILE</type><location>relative-to-java.xml</location></source>
              </teamworkSources>
              <teamworkDefaultCheckIntervalMinutes>45</teamworkDefaultCheckIntervalMinutes>
              <teamworkDefaultCredentialId>old-credential</teamworkDefaultCredentialId>
              <teamworkDefaultSshKeyId>old-key</teamworkDefaultSshKeyId>
              <teamworkDefaultUsername>operator</teamworkDefaultUsername>
              <teamworkUseTemporaryKey>true</teamworkUseTemporaryKey>
            </globalSettings>"#,
            None,
        ).unwrap();
        assert_eq!(import.teamwork_proposals, 7);
        assert_eq!(import.count, 0);
        assert_eq!(import.omitted, 2);
        let incoming = import.contents.unwrap();
        use super::super::backup_data::merge_java_window_settings;
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        let values: serde_json::Value = serde_json::from_slice(&restored).unwrap();
        let proposals: Proposals = serde_json::from_value(values["java_teamwork"].clone()).unwrap();
        assert_eq!(proposals.interval, Some(45));
        assert_eq!(proposals.credential.as_deref(), Some("old-credential"));
        assert_eq!(proposals.key.as_deref(), Some("old-key"));
        assert_eq!(proposals.username.as_deref(), Some("operator"));
        assert_eq!(proposals.temporary_key, Some(true));

        let mut draft = Draft::new(Settings {
            default_check_interval_minutes: 30,
            ..Settings::default()
        });
        assert!(!draft.dirty());
        let source = &proposals.sources[0];
        assert!(source.was_enabled);
        assert_eq!(source.id.as_deref(), Some("java-source"));
        draft.add_java_source(source);
        draft.add_java_source(source);
        let candidate = draft.candidate().unwrap();
        assert_eq!(candidate.sources.len(), 1);
        assert!(!candidate.sources[0].enabled);
        assert_eq!(candidate.sources[0].check_interval_minutes, 20);
        assert_eq!(candidate.default_check_interval_minutes, 30);
        draft.add_java_source(&proposals.sources[1]);
        assert!(
            draft.candidate().is_err(),
            "the shared file needs a local path"
        );
        draft.sources[1].location = location(0);
        assert!(!draft.candidate().unwrap().sources[1].enabled);
    }

    fn location(index: usize) -> String {
        std::env::temp_dir()
            .join(format!("teamwork fixture {index}.xml"))
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn teamwork_settings_is_owner_modal_and_keeps_failed_or_cancelled_edits() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let invoke = |owner| chrome::Message::Invoke(owner, chrome::Action::TeamworkSettings);
        let _ = workspace.update_chrome(invoke(owner));
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        let _ = workspace.update_chrome(invoke(other));
        assert_eq!(workspace.teamwork_settings_for_owner(owner), Some(id));
        assert_eq!(workspace.placed_dialog_owner(id), Some(owner));
        assert!(workspace.teamwork_settings_for_owner(other).is_none());
        assert_eq!(workspace.owner_dialog(owner).map(|(id, _)| id), Some(id));
        assert!(workspace.owner_dialog(other).is_none());
        let _ = workspace.update_teamwork_settings(id, Message::Loaded(Ok(Settings::default())));
        let _ = workspace.update_teamwork_settings(id, Message::Add);
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("an empty source must stay in the editable draft");
        };
        assert!(draft.problem.is_some());

        let path = location(1);
        let _ = workspace.update_teamwork_settings(id, Message::Location(0, path.clone()));
        let _ = workspace.update_teamwork_settings(id, Message::Enable(0, false));
        // Exercise the state transitions without polling the profile-writing task.
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let _ = workspace.close_teamwork_settings(id);
        assert!(matches!(
            workspace.teamwork_settings.0.as_ref().unwrap().phase,
            Phase::Saving(..)
        ));
        let _ = workspace
            .update_teamwork_settings(id, Message::Saved(Err("fixture write error".into())));
        let _ = workspace.close_teamwork_settings(id);
        assert!(matches!(
            workspace.teamwork_settings.0.as_ref().unwrap().phase,
            Phase::ConfirmDiscard(_)
        ));
        let _ = workspace.update_teamwork_settings(id, Message::KeepEditing);
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("keep editing returns to the same draft");
        };
        assert_eq!(
            draft.candidate().unwrap().sources[0],
            Source {
                kind: SourceKind::File,
                location: path,
                enabled: false,
                check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
            }
        );
        assert!(
            draft
                .problem
                .as_deref()
                .unwrap()
                .contains("fixture write error")
        );
        let _ = workspace.close_teamwork_settings(id);
        let _ = workspace.update_teamwork_settings(id, Message::Discard);
        assert!(workspace.teamwork_settings_for_owner(owner).is_none());
    }

    #[test]
    fn teamwork_settings_long_source_list_leaves_the_footer_reachable() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_teamwork_settings(owner);
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        let settings = Settings {
            sources: (0..MAX_SOURCES)
                .map(|index| Source {
                    kind: SourceKind::File,
                    location: location(index),
                    enabled: true,
                    check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
                })
                .collect(),
            ..Settings::default()
        };
        let _ = workspace.update_teamwork_settings(id, Message::Loaded(Ok(settings)));
        let mut view = workspace.teamwork_settings_view(id).unwrap();
        let mut tree = Tree::new(view.as_widget());
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let compact = Size::new(440.0, 245.0);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, compact),
        );
        let column = Layout::new(&node).children().next().unwrap();
        let mut children = column.children();
        let scroll = children.nth(1).unwrap();
        let footer = children.next().unwrap();
        let buttons: Vec<_> = footer.children().next().unwrap().children().collect();
        let footer = footer.bounds();
        assert!(scroll.bounds().height > 0.0);
        assert!(scroll.children().next().unwrap().bounds().height > scroll.bounds().height);
        assert!(footer.height > 0.0);
        assert!(footer.y + footer.height <= compact.height);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].bounds().y, buttons[1].bounds().y);
        let save = buttons[1].bounds();
        assert!((save.x + save.width - footer.x - footer.width).abs() < 0.01);
    }

    #[test]
    fn teamwork_intervals_keep_invalid_input_in_draft_and_save_valid_minutes() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_teamwork_settings(owner);
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        let _ = workspace.update_teamwork_settings(
            id,
            Message::Loaded(Ok(Settings {
                default_check_interval_minutes: 45,
                ..Settings::default()
            })),
        );
        let _ = workspace.update_teamwork_settings(id, Message::Add);
        let _ = workspace.update_teamwork_settings(id, Message::Location(0, location(1)));
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("new source is editable");
        };
        let candidate = draft.candidate().unwrap();
        assert_eq!(candidate.default_check_interval_minutes, 45);
        assert_eq!(candidate.sources[0].kind, SourceKind::File);
        assert_eq!(
            candidate.sources[0].check_interval_minutes,
            DEFAULT_CHECK_INTERVAL_MINUTES
        );
        let _ = workspace.update_teamwork_settings(id, Message::DefaultInterval(String::new()));
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("blank default interval is invalid");
        };
        assert!(draft.default_interval.is_empty());
        assert!(
            draft
                .problem
                .as_deref()
                .unwrap()
                .contains("default check interval")
        );

        let _ = workspace.update_teamwork_settings(id, Message::DefaultInterval("55".into()));
        let _ = workspace.update_teamwork_settings(id, Message::Interval(0, "0".into()));
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("zero source interval is invalid");
        };
        assert_eq!(draft.sources[0].interval, "0");
        assert!(draft.problem.as_deref().unwrap().contains("source 1"));

        let _ = workspace.update_teamwork_settings(id, Message::Interval(0, "1440".into()));
        // Inspect the committed candidate without polling the profile-writing task.
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Saving(_, candidate) = &workspace.teamwork_settings.0.as_ref().unwrap().phase
        else {
            panic!("valid default and source intervals can be saved");
        };
        assert_eq!(candidate.default_check_interval_minutes, 55);
        assert_eq!(candidate.sources[0].check_interval_minutes, 1440);
    }

    #[test]
    fn teamwork_settings_policy_blocks_open_and_save_but_unchanged_valid_settings_can_refresh() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.clipboard.policy.teamwork_allowed = false;
        let _ = workspace.open_teamwork_settings(owner);
        assert!(workspace.teamwork_settings_for_owner(owner).is_none());

        workspace.clipboard.policy.teamwork_allowed = true;
        let _ = workspace.open_teamwork_settings(owner);
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        let saved = Settings::default();
        let _ = workspace.update_teamwork_settings(id, Message::Loaded(Ok(saved.clone())));
        workspace.clipboard.policy.custom_teamwork_sources_allowed = false;
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        assert!(matches!(
            workspace.teamwork_settings.0.as_ref().unwrap().phase,
            Phase::Editing(_)
        ));

        workspace.clipboard.policy.custom_teamwork_sources_allowed = true;
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Saving(draft, candidate) =
            &workspace.teamwork_settings.0.as_ref().unwrap().phase
        else {
            panic!("Save requests a refresh even without edits");
        };
        assert!(!draft.dirty());
        assert_eq!(candidate, &saved);
    }

    #[test]
    fn teamwork_settings_preserves_legacy_file_and_saves_custom_git_separately_from_policy() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let legacy_file = location(1);
        let saved: Settings = serde_json::from_value(serde_json::json!({
            "sources": [{"location": legacy_file, "enabled": true}],
        }))
        .unwrap();
        assert_eq!(saved.sources[0].kind, SourceKind::File);
        workspace.clipboard.policy.teamwork_sources.push(managed());
        let _ = workspace.open_teamwork_settings(owner);
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        let _ = workspace.update_teamwork_settings(id, Message::Loaded(Ok(saved.clone())));
        let _ = workspace.update_teamwork_settings(id, Message::SourceType(0, SourceKind::File));
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("legacy source is editable");
        };
        assert!(!draft.dirty());
        assert_eq!(draft.candidate().unwrap(), saved);

        let _ = workspace.update_teamwork_settings(id, Message::SourceType(0, SourceKind::Git));
        let Phase::Editing(draft) = &workspace.teamwork_settings.0.as_ref().unwrap().phase else {
            panic!("switching types keeps the draft open");
        };
        assert!(draft.dirty());
        assert!(draft.sources[0].location.is_empty());
        assert!(draft.candidate().is_err());

        let url = "https://git.example.test/team/custom.git";
        let _ = workspace.update_teamwork_settings(id, Message::Location(0, url.into()));
        let _ = workspace.update_teamwork_settings(id, Message::Save);
        let Phase::Saving(_, candidate) = &workspace.teamwork_settings.0.as_ref().unwrap().phase
        else {
            panic!("valid Git source is ready to save");
        };
        assert_eq!(candidate.sources.len(), 1);
        assert_eq!(candidate.sources[0].kind, SourceKind::Git);
        assert_eq!(candidate.sources[0].location, url);
    }

    #[test]
    fn teamwork_settings_managed_only_is_read_only_and_can_refresh() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.clipboard.policy.custom_teamwork_sources_allowed = false;
        workspace.clipboard.policy.teamwork_sources.push(managed());
        let _ = workspace.open_teamwork_settings(owner);
        let id = workspace.teamwork_settings_for_owner(owner).unwrap();
        assert!(matches!(
            workspace.teamwork_settings.0.as_ref().unwrap().phase,
            Phase::Managed
        ));
        assert!(workspace.teamwork_settings_view(id).is_some());
        for message in [
            Message::Retry,
            Message::Loaded(Ok(Settings::default())),
            Message::Add,
            Message::SourceType(0, SourceKind::File),
            Message::Remove(0),
            Message::Save,
        ] {
            let _ = workspace.update_teamwork_settings(id, message);
        }
        assert!(matches!(
            workspace.teamwork_settings.0.as_ref().unwrap().phase,
            Phase::Managed
        ));
        assert!(workspace.guard_teamwork_settings_quit().is_none());
        assert_eq!(workspace.clipboard.policy.teamwork_sources, [managed()]);
        let _ = workspace.update_teamwork_settings(id, Message::Refresh);
        assert!(workspace.teamwork_settings_for_owner(owner).is_none());
    }

    fn managed() -> ManagedSource {
        ManagedSource {
            name: "Operations".into(),
            source: Source {
                kind: SourceKind::Git,
                location: "https://git.example.test/team/managed.git".into(),
                enabled: true,
                check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
            },
        }
    }
}
