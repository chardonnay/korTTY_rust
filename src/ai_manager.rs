//! Native AI Manager and its profile-scoped settings.

use std::fmt;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{Id, button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use serde::{Deserialize, Serialize};

use crate::{local_ai, profile};

mod local;
mod memory;
mod profiles;
mod skills;

pub(crate) use memory::Scope;

pub(crate) const TITLE: &str = "AI Manager";
pub(crate) const FILE_NAME: &str = "ai-manager.json";
const VERSION: u8 = 1;
const MAX_TIMEOUT_MINUTES: u16 = 1440;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Configuration {
    version: u8,
    general: General,
    profiles: profiles::Catalog,
    local: local::State,
    skills: skills::Store,
    memory: memory::Settings,
}

impl fmt::Debug for Configuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiConfiguration")
            .finish_non_exhaustive()
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: VERSION,
            general: General::default(),
            profiles: profiles::Catalog::default(),
            local: local::State::default(),
            skills: skills::Store::default(),
            memory: memory::Settings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct General {
    features_enabled: bool,
    timeout_minutes: u16,
}

impl Default for General {
    fn default() -> Self {
        Self {
            features_enabled: true,
            timeout_minutes: 1,
        }
    }
}

impl Configuration {
    pub(crate) fn load() -> io::Result<Self> {
        let path = profile::root()?.join(FILE_NAME);
        let file = match profile::open_preference(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !file.metadata()?.is_file() {
            return Err(invalid("AI settings require a regular file"));
        }
        let mut content = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut content)?;
        Self::decode(&content)
    }

    fn decode(content: &[u8]) -> io::Result<Self> {
        if content.len() as u64 > MAX_BYTES {
            return Err(invalid("AI settings exceed the size limit"));
        }
        let value: Self = serde_json::from_slice(content).map_err(invalid)?;
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || !(1..=MAX_TIMEOUT_MINUTES).contains(&self.general.timeout_minutes)
        {
            return Err(invalid("Invalid AI settings version or request timeout"));
        }
        self.profiles.validate().map_err(invalid)?;
        self.local.validate().map_err(invalid)?;
        self.skills.validate().map_err(invalid)
    }

    fn save(&self) -> io::Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("AI settings exceed the size limit"));
        }
        profile::save_atomic(&profile::root()?.join(FILE_NAME), &bytes)
    }

    fn profile_for(&self, role: local::Role) -> Option<&profiles::Profile> {
        self.local
            .profile(role)
            .and_then(|id| self.profiles.profile(id))
            .or_else(|| self.profiles.default_profile())
    }

    pub(crate) fn drawing_profile(&self, code: bool) -> Option<ManagedProfile> {
        let role = if code {
            local::Role::Coding
        } else {
            local::Role::Text
        };
        let entry = self.profile_for(role)?;
        Some(ManagedProfile {
            endpoint: local_ai::numeric_loopback_endpoint(&entry.endpoint)
                .map_or_else(|_| entry.endpoint.clone(), |address| address.to_string()),
            model: entry.model.clone(),
            transport: entry.transport(self.general.timeout_minutes),
            selection_limit: entry.selection_limit as usize,
        })
    }

    pub(crate) fn journal_profile_name(&self) -> Option<&str> {
        let id = self.local.profile(local::Role::Journal)?;
        Some(&self.profiles.profile(id)?.name)
    }

    pub(crate) fn journal_profile(
        &self,
        vision: bool,
    ) -> Option<Result<local_ai::Profile, String>> {
        let id = self.local.profile(local::Role::Journal)?;
        Some(
            self.profiles
                .profile(id)
                .ok_or_else(|| "The selected journal AI profile was removed.".into())
                .and_then(|profile| {
                    if vision && profile.vision == profiles::Vision::Disabled {
                        return Err(
                            "Vision is disabled for the selected journal AI profile.".into()
                        );
                    }
                    profile.transport(self.general.timeout_minutes)
                }),
        )
    }

    pub(crate) fn skill_guidance(&self, context: &str) -> String {
        self.skills.prompt_addendum(context, skills::Target::Chat)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.general.features_enabled
    }

    pub(crate) fn code_analysis_profile_id(&self) -> Option<u64> {
        Some(self.profile_for(local::Role::Coding)?.id)
    }

    pub(crate) fn code_analysis_profiles(&self) -> impl Iterator<Item = (u64, &str, &str, String)> {
        self.profiles.entries().filter_map(|entry| {
            entry.transport(self.general.timeout_minutes).ok()?;
            let endpoint = local_ai::numeric_loopback_endpoint(&entry.endpoint).ok()?;
            Some((
                entry.id,
                entry.name.as_str(),
                entry.model.as_str(),
                endpoint.to_string(),
            ))
        })
    }

    pub(crate) fn code_analysis_transport(
        &self,
        id: u64,
    ) -> Result<(local_ai::Profile, usize), String> {
        if !self.enabled() {
            return Err("AI features are disabled in AI Manager.".into());
        }
        let entry = self
            .profiles
            .profile(id)
            .ok_or("The selected AI profile was removed.")?;
        Ok((
            entry.transport(self.general.timeout_minutes)?,
            entry.selection_limit as usize,
        ))
    }
}

pub(crate) struct ManagedProfile {
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) transport: Result<local_ai::Profile, String>,
    pub(crate) selection_limit: usize,
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    #[default]
    General,
    Profiles,
    LocalModels,
    Knowledge,
    Skills,
    Memory,
    LocalPreferences,
}

impl Page {
    const ALL: [Self; 7] = [
        Self::General,
        Self::Profiles,
        Self::LocalModels,
        Self::Knowledge,
        Self::Skills,
        Self::Memory,
        Self::LocalPreferences,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Profiles => "Profiles",
            Self::LocalModels => "Local Models",
            Self::Knowledge => "Knowledge Stores",
            Self::Skills => "AI Skills",
            Self::Memory => "AI Memory",
            Self::LocalPreferences => "Local Preferences",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::General => "enable features timeout agent internet policies global request",
            Self::Profiles => {
                "provider API URL key connection model reasoning vision limits default"
            }
            Self::LocalModels => "server discovery loopback connection installed model",
            Self::Knowledge => "embedding documents files search retrieval index",
            Self::Skills => "prompt instructions rules markdown import export",
            Self::Memory => "global connection history learned archive restore",
            Self::LocalPreferences => "default profiles text coding agent journal",
        }
    }

    fn step(self, next: bool) -> Option<Self> {
        let position = Self::ALL.iter().position(|&page| page == self)?;
        let target = if next {
            position + 1
        } else {
            position.checked_sub(1)?
        };
        Self::ALL.get(target).copied()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Page(Page),
    Search(String),
    Find,
    Help,
    Features(bool),
    Timeout(String),
    Profiles(profiles::Message),
    Skills(skills::Message),
    Memory(memory::Message),
    Local(local::Message),
    RequestDone(u64, Result<Reply, String>),
    CancelRequest,
    Save,
    Saved(Result<Arc<Configuration>, String>),
    Close,
    CloseAnswer(bool),
}

#[derive(Clone, Debug)]
pub(crate) enum Reply {
    Models(Vec<String>),
    Tested(String),
}

struct Request {
    ticket: u64,
    profile: profiles::Profile,
    test_after_discovery: bool,
    _cancel: local_ai::Cancel,
}

pub(crate) struct Dialog {
    saved: Arc<Configuration>,
    general: General,
    timeout: String,
    profiles: profiles::Editor,
    skills: skills::Editor,
    skill_store: skills::Store,
    memory: memory::State,
    local: local::Dialog,
    local_state: local::State,
    page: Page,
    search: String,
    search_id: Id,
    page_scroll: Id,
    notice: String,
    editable: bool,
    saving: bool,
    request: Option<Request>,
    next_request: u64,
    close_token: Option<u64>,
}

impl Dialog {
    pub(crate) fn new(
        saved: Arc<Configuration>,
        directory: PathBuf,
        scopes: Vec<Scope>,
        editable: bool,
        problem: Option<&str>,
    ) -> Self {
        Self {
            general: saved.general.clone(),
            timeout: saved.general.timeout_minutes.to_string(),
            profiles: profiles::Editor::new(saved.profiles.clone()),
            skills: skills::Editor::default(),
            skill_store: saved.skills.clone(),
            memory: memory::State::new(saved.memory.clone(), directory, scopes, editable),
            local: local::Dialog::default(),
            local_state: saved.local.clone(),
            saved,
            page: Page::default(),
            search: String::new(),
            search_id: Id::unique(),
            page_scroll: Id::unique(),
            notice: problem.unwrap_or_default().into(),
            editable,
            saving: false,
            request: None,
            next_request: 0,
            close_token: None,
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        self.general != self.saved.general
            || self.timeout != self.saved.general.timeout_minutes.to_string()
            || self.profiles.catalog() != &self.saved.profiles
            || self.profiles.draft_dirty()
            || self.skill_store != self.saved.skills
            || self.skills.pending_edit()
            || self.memory.is_dirty()
            || self.local_state != self.saved.local
    }

    pub(crate) fn busy(&self) -> bool {
        self.saving || self.skills.busy() || self.memory.busy()
    }

    pub(crate) fn prompt_close(&mut self, token: Option<u64>) {
        self.close_token = token;
    }

    pub(crate) fn close_token(&self) -> Option<u64> {
        self.close_token
    }

    fn editable(&self) -> bool {
        self.editable && !self.saving && self.close_token.is_none()
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let editable = self.editable();
        match message {
            Message::Page(page) => {
                self.page = page;
                self.search.clear();
                let reset = iced::widget::operation::snap_to(
                    self.page_scroll.clone(),
                    iced::widget::operation::RelativeOffset::START,
                );
                return if page == Page::Memory {
                    Task::batch([reset, self.memory.load().map(Message::Memory)])
                } else {
                    reset
                };
            }
            Message::Search(value) => self.search = value.chars().take(128).collect(),
            Message::Find => return iced::widget::operation::focus(self.search_id.clone()),
            Message::Features(value) if editable => {
                self.general.features_enabled = value;
                if !value {
                    self.request = None;
                    self.local.suspend();
                }
            }
            Message::Timeout(value) if editable => {
                self.timeout = value.chars().filter(char::is_ascii_digit).take(4).collect();
            }
            Message::Profiles(profiles::Message::Test) if editable => {
                return self.request(false);
            }
            Message::Profiles(profiles::Message::Discover) if editable => {
                return self.request(true);
            }
            Message::Profiles(profiles::Message::Save) | Message::Save
                if editable && !self.busy() && self.request.is_none() =>
            {
                return self.save();
            }
            Message::Profiles(profiles::Message::Refresh) if editable => {
                if self.profiles.catalog() == &self.saved.profiles && !self.profiles.draft_dirty() {
                    self.profiles.replace(self.saved.profiles.clone());
                    self.profiles.set_status("Profile list refreshed.");
                } else {
                    self.profiles
                        .set_status("Save your profile edits before refreshing.");
                }
            }
            Message::Profiles(event) => self.profiles.update(event, editable),
            Message::Skills(event)
                if editable
                    || matches!(
                        event,
                        skills::Message::Imported(_)
                            | skills::Message::Exported(_)
                            | skills::Message::Shortened(..)
                    ) =>
            {
                let profile = matches!(event, skills::Message::Shorten)
                    .then(|| self.shortening_profile())
                    .flatten();
                return self
                    .skills
                    .update(&mut self.skill_store, event, profile)
                    .map(Message::Skills);
            }
            Message::Memory(event)
                if editable || matches!(event, memory::Message::Completed(_)) =>
            {
                return self.memory.update(event).map(Message::Memory);
            }
            Message::Local(event) if editable || matches!(event, local::Message::Finished(..)) => {
                return self
                    .local
                    .update(
                        event,
                        &mut self.local_state,
                        self.editable && self.general.features_enabled,
                    )
                    .map(Message::Local);
            }
            Message::CancelRequest => {
                self.request = None;
                self.notice = "Request cancelled.".into();
            }
            Message::RequestDone(ticket, result) => {
                if self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.ticket == ticket)
                    && let Some(request) = self.request.take()
                {
                    if self.profiles.selected() != Some(&request.profile) {
                        self.notice =
                            "Profile changed. Run the request again for the new settings.".into();
                        return Task::none();
                    }
                    self.notice = match result {
                        Ok(Reply::Models(models)) => {
                            let count = models.len();
                            self.profiles.set_models(
                                request.profile.id,
                                request.profile.endpoint.trim(),
                                models,
                            );
                            if request.test_after_discovery && count > 0 {
                                return self.request(false);
                            }
                            if count == 1 {
                                "Found one model.".into()
                            } else {
                                format!("Found {count} models.")
                            }
                        }
                        Ok(Reply::Tested(model)) => {
                            format!("Model responded successfully ({model}).")
                        }
                        Err(error) => error,
                    };
                }
            }
            Message::Saved(result) => {
                self.saving = false;
                self.notice = match result {
                    Ok(saved) => {
                        self.general = saved.general.clone();
                        self.timeout = self.general.timeout_minutes.to_string();
                        self.profiles.replace(saved.profiles.clone());
                        self.saved = saved;
                        self.memory.mark_saved();
                        "AI settings saved.".into()
                    }
                    Err(error) => format!("Could not save AI settings ({error})."),
                };
            }
            _ => {}
        }
        Task::none()
    }

    fn save(&mut self) -> Task<Message> {
        let snapshot = (|| {
            let timeout_minutes = self
                .timeout
                .parse::<u16>()
                .ok()
                .filter(|value| (1..=MAX_TIMEOUT_MINUTES).contains(value))
                .ok_or("Enter a request timeout from 1 to 1440 minutes.")?;
            if !self.memory.settings_valid() {
                return Err("Correct the AI Memory token budget first.".to_owned());
            }
            let profiles = self.profiles.validated_catalog()?;
            self.local_state
                .retain_profiles(|id| profiles.profile(id).is_some());
            self.skills.commit(&mut self.skill_store)?;
            Ok::<_, String>(Arc::new(Configuration {
                version: VERSION,
                general: General {
                    timeout_minutes,
                    ..self.general.clone()
                },
                profiles,
                local: self.local_state.clone(),
                skills: self.skill_store.clone(),
                memory: self.memory.settings().clone(),
            }))
        })();
        let saved = match snapshot {
            Ok(saved) => saved,
            Err(error) => {
                self.notice = error;
                return Task::none();
            }
        };
        self.saving = true;
        Task::perform(
            async move {
                saved
                    .save()
                    .map(|()| saved)
                    .map_err(|error| error.to_string())
            },
            Message::Saved,
        )
    }

    fn request(&mut self, discover: bool) -> Task<Message> {
        if !self.general.features_enabled || self.request.is_some() {
            self.notice = "Enable AI features and finish the current request first.".into();
            return Task::none();
        }
        let result = self
            .profiles
            .selected()
            .ok_or_else(|| "Select a profile first.".to_owned())
            .and_then(|profile| {
                let endpoint = profile.endpoint.trim().to_owned();
                let test_after_discovery = !discover
                    && profile.model_mode == profiles::ModelMode::Auto
                    && profile.model.trim().is_empty();
                let pending = if discover || test_after_discovery {
                    let pending = match profile.credential()? {
                        Some(name) => local_ai::discover_with_credential(&endpoint, Some(name))?,
                        None => local_ai::discover(&endpoint)?,
                    };
                    let local_ai::Pending { reply, cancel } = pending;
                    (
                        Task::future(async move {
                            reply
                                .recv()
                                .await
                                .unwrap_or_else(|_| Err("The worker stopped.".into()))
                                .map(Reply::Models)
                        }),
                        cancel,
                    )
                } else {
                    let timeout = self
                        .timeout
                        .parse()
                        .ok()
                        .filter(|value| (1..=MAX_TIMEOUT_MINUTES).contains(value))
                        .ok_or("Enter a valid global request timeout before testing.")?;
                    let config = profile.transport(timeout)?;
                    let pending = local_ai::start(config, |client| {
                        client.complete_text_before(
                            "You are testing a connection. Respond with a short acknowledgement.",
                            "Reply with OK.",
                            false,
                            Instant::now() + Duration::from_secs(86_400),
                        ).map_err(local_ai::Error::message)?;
                        Ok(Reply::Tested(client.model().into()))
                    })?;
                    let local_ai::Pending { reply, cancel } = pending;
                    (
                        Task::future(async move {
                            reply
                                .recv()
                                .await
                                .unwrap_or_else(|_| Err("The worker stopped.".into()))
                        }),
                        cancel,
                    )
                };
                Ok((profile.clone(), test_after_discovery, pending))
            });
        match result {
            Ok((profile, test_after_discovery, (reply, cancel))) => {
                let ticket = self.next_request;
                self.next_request = self.next_request.wrapping_add(1);
                self.request = Some(Request {
                    ticket,
                    profile,
                    test_after_discovery,
                    _cancel: cancel,
                });
                self.notice = if discover || test_after_discovery {
                    "Loading models…"
                } else {
                    "Testing connection…"
                }
                .into();
                reply.map(move |result| Message::RequestDone(ticket, result))
            }
            Err(error) => {
                self.notice = error;
                Task::none()
            }
        }
    }

    fn shortening_profile(&self) -> Option<local_ai::Profile> {
        if !self.editable() || !self.general.features_enabled {
            return None;
        }
        self.saved.drawing_profile(false)?.transport.ok()
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let controls = row![
            button("‹").on_press_maybe(self.page.step(false).map(Message::Page)),
            text(self.page.title()),
            button("›").on_press_maybe(self.page.step(true).map(Message::Page)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let header = row![
            text_input("Search AI settings…", &self.search)
                .id(self.search_id.clone())
                .on_input(Message::Search),
            controls,
            button("Manual").style(button::text).on_press(Message::Help),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let tabs = scrollable(
            row(Page::ALL.into_iter().map(|page| {
                button(page.title())
                    .style(if page == self.page {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::Page(page))
                    .into()
            }))
            .spacing(3),
        )
        .horizontal()
        .width(Length::Fill);
        let profiles = self.profiles.catalog().choices();
        let content: Element<'_, Message> = if self.search.trim().is_empty() {
            match self.page {
                Page::General => self.general_view(),
                Page::Profiles => self.profiles.view(self.editable()).map(Message::Profiles),
                Page::Skills if self.editable() => self
                    .skills
                    .view(&self.skill_store, self.shortening_profile().is_some())
                    .map(Message::Skills),
                Page::Memory => self.memory.view().map(Message::Memory),
                Page::Skills => text("AI skill editing is unavailable.").into(),
                Page::LocalModels | Page::Knowledge | Page::LocalPreferences => {
                    let page = match self.page {
                        Page::LocalModels => local::Page::Models,
                        Page::Knowledge => local::Page::Knowledge,
                        _ => local::Page::Preferences,
                    };
                    self.local
                        .view(
                            page,
                            &self.local_state,
                            &profiles,
                            self.editable() && self.general.features_enabled,
                        )
                        .map(Message::Local)
                }
            }
        } else {
            self.search_view()
        };
        let footer = row![
            text(self.notice.as_str()).size(13).width(Length::Fill),
            self.request
                .as_ref()
                .map(|_| button("Cancel request").on_press(Message::CancelRequest)),
            (self.page != Page::Profiles).then(|| button(if self.saving {
                "Saving…"
            } else {
                "Save"
            })
            .on_press_maybe(
                (self.editable() && !self.busy() && self.request.is_none() && self.dirty())
                    .then_some(Message::Save)
            )),
            button("Close")
                .style(button::secondary)
                .on_press_maybe((!self.busy()).then_some(Message::Close)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let content: Element<'_, Message> = if !self.search.trim().is_empty()
            || matches!(self.page, Page::General | Page::Memory)
        {
            scrollable(content)
                .id(self.page_scroll.clone())
                .height(Length::Fill)
                .into()
        } else {
            container(content).height(Length::Fill).into()
        };
        let body = column![header, tabs, content, footer]
            .spacing(10)
            .padding(14)
            .height(Length::Fill);
        if self.close_token.is_some() {
            iced::widget::stack![
                body,
                iced::widget::opaque(
                    container(
                        column![
                            text("Discard unsaved AI Manager changes?").size(18),
                            row![
                                button("Keep editing").on_press(Message::CloseAnswer(false)),
                                button("Discard").style(button::danger).on_press_maybe(
                                    (!self.busy()).then_some(Message::CloseAnswer(true))
                                ),
                            ]
                            .spacing(8)
                        ]
                        .spacing(14)
                        .padding(18),
                    )
                    .center(Length::Fill)
                    .style(container::dark)
                )
            ]
            .into()
        } else {
            body.into()
        }
    }

    fn general_view(&self) -> Element<'_, Message> {
        let mut features = checkbox(self.general.features_enabled).label("Enable AI features");
        let mut timeout = text_input("1–1440", &self.timeout).width(80);
        if self.editable() {
            features = features.on_toggle(Message::Features);
            timeout = timeout.on_input(Message::Timeout);
        }
        column![
            text("Features").size(19),
            features,
            text("AI requests use explicitly configured local endpoints. Local diagram generation remains available."),
            text("Limits").size(19),
            row![text("Global request timeout (minutes)"), timeout].spacing(12).align_y(Alignment::Center),
            text("Internet and terminal agents").size(19),
            text("Internet providers and terminal agent execution require additional integrations.").size(13),
        ].spacing(12).into()
    }

    fn search_view(&self) -> Element<'_, Message> {
        let query = self.search.trim().to_lowercase();
        let mut results = column![text("Matching pages").size(17)].spacing(8);
        let mut found = false;
        for page in Page::ALL {
            let haystack = format!("{} {}", page.title().to_lowercase(), page.keywords());
            if query.split_whitespace().all(|word| haystack.contains(word)) {
                found = true;
                results = results.push(button(page.title()).on_press(Message::Page(page)));
            }
        }
        if !found {
            results = results.push(text("No matching settings."));
        }
        results.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_roles_round_trip_and_require_vision_for_screenshots() -> io::Result<()> {
        let original = Configuration::decode(br#"{
            "profiles": {
                "profiles": [
                    {"id": 1, "name": "Text", "endpoint": "127.0.0.1:11434", "model": "text"},
                    {"id": 2, "name": "Code", "endpoint": "127.0.0.1:11435", "model": "code", "vision": "disabled"}
                ],
                "default": 1,
                "last_id": 2
            },
            "local": {"roles": [null, 2, null, 2]}
        }"#)?;
        let serialized = serde_json::to_vec(&original)?;
        let restored = Configuration::decode(&serialized)?;
        assert_eq!(original, restored);
        assert_eq!(restored.drawing_profile(false).unwrap().model, "text");
        assert_eq!(restored.drawing_profile(true).unwrap().model, "code");
        assert!(matches!(restored.journal_profile(false), Some(Ok(_))));
        assert!(matches!(restored.journal_profile(true), Some(Err(_))));
        Ok(())
    }

    #[test]
    fn config_rejects_remote_profiles_and_unknown_versions() {
        assert!(Configuration::decode(br#"{"version":2}"#).is_err());
        assert!(Configuration::decode(br#"{
            "profiles": {
                "profiles":[{"id":1,"name":"Remote","endpoint":"https://example.invalid/v1","model":"x"}],
                "default":1,"last_id":1
            }
        }"#).is_err());
    }
}
