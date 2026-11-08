//! Local model discovery, profile roles and knowledge-store configuration.

mod knowledge;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use serde::{Deserialize, Serialize};

use crate::local_ai::{self, Pending, Profile};

use knowledge::{Index, Store};

#[derive(Clone, Copy)]
pub(super) enum Page {
    Models,
    Knowledge,
    Preferences,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Role {
    Text,
    Coding,
    Agent,
    Journal,
}

impl Role {
    const ALL: [Self; 4] = [Self::Text, Self::Coding, Self::Agent, Self::Journal];

    fn label(self) -> &'static str {
        match self {
            Self::Text => "Text profile",
            Self::Coding => "Coding profile",
            Self::Agent => "Terminal agent profile",
            Self::Journal => "Session journal profile",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct State {
    roles: [Option<u64>; 4],
    embedding: String,
    stores: Vec<Store>,
}

impl State {
    pub(super) fn profile(&self, role: Role) -> Option<u64> {
        self.roles[role as usize]
    }

    pub(super) fn retain_profiles(&mut self, has_profile: impl Fn(u64) -> bool) {
        for role in &mut self.roles {
            if role.is_some_and(|id| !has_profile(id)) {
                *role = None;
            }
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.stores.len() > knowledge::MAX_STORES
            || self.embedding.len() > local_ai::MAX_MODEL_BYTES
            || self.embedding.chars().any(char::is_control)
        {
            return Err("Invalid local AI settings".into());
        }
        for (index, store) in self.stores.iter().enumerate() {
            store.validate()?;
            if self.stores[..index]
                .iter()
                .any(|other| other.name == store.name)
            {
                return Err("Knowledge store names must be unique".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Choice(Option<u64>, String);

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.1)
    }
}

pub(super) struct Dialog {
    endpoint: String,
    models: Vec<String>,
    model: Option<String>,
    status: Option<String>,
    request: Option<local_ai::Cancel>,
    generation: u64,
    selected: Option<usize>,
    store_name: String,
    source: String,
    confirm_remove: bool,
    consent: bool,
    query: String,
    index: Option<Arc<Index>>,
    hits: Vec<String>,
}

impl Default for Dialog {
    fn default() -> Self {
        Self {
            endpoint: local_ai::DEFAULT_ENDPOINT.into(),
            models: Vec::new(),
            model: None,
            status: None,
            request: None,
            generation: 0,
            selected: None,
            store_name: String::new(),
            source: String::new(),
            confirm_remove: false,
            consent: false,
            query: String::new(),
            index: None,
            hits: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Endpoint(String),
    Discover,
    Model(String),
    TestModel,
    Role(Role, Option<u64>),
    Embedding(String),
    StoreName(String),
    CreateStore,
    SelectStore(usize),
    ConfirmRemove(bool),
    RemoveStore,
    Source(String),
    AddSource,
    RemoveSource(usize),
    Consent(bool),
    Index,
    Query(String),
    Search,
    Cancel,
    Finished(u64, Result<Output, String>),
}

#[derive(Clone, Debug)]
pub(crate) enum Output {
    Models(Vec<String>),
    ModelTest,
    Index(Arc<Index>),
    Hits { query: String, hits: Vec<String> },
}

impl Dialog {
    pub(super) fn update(
        &mut self,
        message: Message,
        state: &mut State,
        ai_allowed: bool,
    ) -> Task<Message> {
        match message {
            Message::Endpoint(value) => {
                self.endpoint = limited(&value, 128).into();
                self.models.clear();
                self.model = None;
                self.invalidate_index();
                self.consent = false;
            }
            Message::Discover if ai_allowed && self.request.is_none() => {
                return self.run(
                    local_ai::discover_with_credential(&self.endpoint, None),
                    Output::Models,
                );
            }
            Message::Model(model) => {
                if self.models.contains(&model) {
                    self.model = Some(model);
                }
            }
            Message::TestModel if ai_allowed && self.request.is_none() => {
                if let Some(model) = &self.model {
                    let pending = self.with_model(model, |client| {
                        client
                            .complete_text_before(
                                "Reply briefly.",
                                "This is a connectivity test. Reply OK.",
                                false,
                                Instant::now() + Duration::from_secs(30),
                            )
                            .map(|_| ())
                            .map_err(local_ai::Error::message)
                    });
                    return self.run(pending, |()| Output::ModelTest);
                }
            }
            Message::Role(role, profile) => state.roles[role as usize] = profile,
            Message::Embedding(model) => {
                state.embedding = limited(&model, local_ai::MAX_MODEL_BYTES).into();
                self.invalidate_index();
                self.consent = false;
            }
            Message::StoreName(name) => self.store_name = limited(&name, 128).into(),
            Message::CreateStore => {
                let name = self.store_name.trim();
                if !name.is_empty()
                    && !name.chars().any(char::is_control)
                    && state.stores.len() < knowledge::MAX_STORES
                    && !state.stores.iter().any(|store| store.name == name)
                {
                    state.stores.push(Store::new(name));
                    self.select_store(Some(state.stores.len() - 1));
                    self.store_name.clear();
                } else {
                    self.status =
                        Some("Enter a unique store name. Up to 32 stores are supported.".into());
                }
            }
            Message::SelectStore(index) if index < state.stores.len() => {
                self.select_store(Some(index));
            }
            Message::ConfirmRemove(value) => self.confirm_remove = value,
            Message::RemoveStore if self.confirm_remove => {
                if let Some(index) = self.selected.filter(|&index| index < state.stores.len()) {
                    state.stores.remove(index);
                    self.select_store(None);
                }
            }
            Message::Source(value) => self.source = limited(&value, 4096).into(),
            Message::AddSource => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index)) {
                    match store.add_source(PathBuf::from(self.source.trim())) {
                        Ok(()) => {
                            self.source.clear();
                            self.invalidate_index();
                            self.consent = false;
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
            }
            Message::RemoveSource(source) => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get_mut(index))
                    && source < store.sources.len()
                {
                    store.sources.remove(source);
                    self.invalidate_index();
                }
            }
            Message::Consent(consent) => {
                self.consent = consent;
                if !consent {
                    self.invalidate_index();
                }
            }
            Message::Index if ai_allowed && self.consent && self.request.is_none() => {
                if let Some(store) = self.selected.and_then(|index| state.stores.get(index)) {
                    let store = store.clone();
                    let pending = self.with_model(&state.embedding, move |client| {
                        Index::build(client, &store).map(Arc::new)
                    });
                    return self.run(pending, Output::Index);
                }
            }
            Message::Query(query) => {
                self.query = limited(&query, 2048).into();
                self.hits.clear();
            }
            Message::Search if ai_allowed && self.consent && self.request.is_none() => {
                if let Some(index) = &self.index {
                    let index = Arc::clone(index);
                    let query = self.query.trim().to_owned();
                    let pending = self.with_model(&state.embedding, move |client| {
                        let hits = index.search(client, &query)?;
                        Ok(Output::Hits { query, hits })
                    });
                    return self.run(pending, |output| output);
                }
            }
            Message::Cancel => self.cancel(),
            Message::Finished(generation, result) if generation == self.generation => {
                self.request.take();
                if !ai_allowed {
                    self.invalidate_index();
                    return Task::none();
                }
                self.status = Some(match result {
                    Ok(Output::Models(models)) => {
                        self.model = self.model.take().filter(|model| models.contains(model));
                        self.models = models;
                        format!("{} installed models", self.models.len())
                    }
                    Ok(Output::ModelTest) => "The model replied to the connection test.".into(),
                    Ok(Output::Index(index)) => {
                        let count = index.len();
                        self.index = Some(index);
                        self.hits.clear();
                        format!("Indexed {count} text passages for this dialog.")
                    }
                    Ok(Output::Hits { query, hits }) if query == self.query.trim() => {
                        self.hits = hits;
                        format!("{} matching passages", self.hits.len())
                    }
                    Ok(Output::Hits { .. }) => {
                        self.status = None;
                        return Task::none();
                    }
                    Err(error) => error,
                });
            }
            _ => {}
        }
        Task::none()
    }

    fn with_model<T: Send + 'static>(
        &self,
        model: &str,
        run: impl FnOnce(&local_ai::Client) -> Result<T, String> + Send + 'static,
    ) -> Result<Pending<T>, String> {
        local_ai::start(Profile::new(&self.endpoint, model)?, run)
    }

    fn run<T: Send + 'static>(
        &mut self,
        pending: Result<Pending<T>, String>,
        output: impl FnOnce(T) -> Output + Send + 'static,
    ) -> Task<Message> {
        let Pending { reply, cancel } = match pending {
            Ok(pending) => pending,
            Err(error) => {
                self.status = Some(error);
                return Task::none();
            }
        };
        self.generation = self.generation.wrapping_add(1);
        self.request = Some(cancel);
        self.status = Some("Working…".into());
        let generation = self.generation;
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The local AI worker stopped".into()))
                    .map(output)
            },
            move |result| Message::Finished(generation, result),
        )
    }

    fn cancel(&mut self) {
        if self.request.take().is_some() {
            self.generation = self.generation.wrapping_add(1);
            self.status = Some("Cancelled.".into());
        }
    }

    fn invalidate_index(&mut self) {
        self.cancel();
        self.index = None;
        self.hits.clear();
    }

    pub(super) fn suspend(&mut self) {
        self.invalidate_index();
        self.consent = false;
    }

    fn select_store(&mut self, index: Option<usize>) {
        self.invalidate_index();
        self.selected = index;
        self.confirm_remove = false;
        self.consent = false;
        self.status = None;
        self.source.clear();
        self.query.clear();
    }

    pub(super) fn view<'a>(
        &'a self,
        page: Page,
        state: &'a State,
        profiles: &[(u64, String)],
        ai_allowed: bool,
    ) -> Element<'a, Message> {
        let can_request = ai_allowed && self.request.is_none();
        let page = match page {
            Page::Models => self.models_view(can_request),
            Page::Knowledge => self.knowledge_view(state, can_request),
            Page::Preferences => preferences(state, profiles),
        };
        let progress = row![
            self.status
                .as_ref()
                .map(|status| text(status.as_str()).width(Length::Fill)),
            self.request
                .as_ref()
                .map(|_| button("Cancel request").on_press(Message::Cancel)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![scrollable(page).height(Length::Fill), progress,]
            .spacing(10)
            .into()
    }

    fn models_view(&self, can_request: bool) -> Element<'_, Message> {
        let models = pick_list(self.models.as_slice(), self.model.as_ref(), Message::Model)
            .placeholder("Select an installed model")
            .width(Length::Fill);
        column![
            text("Local Models").size(20),
            text("Discover installed models from an explicitly configured local service."),
            self.endpoint_field(),
            row![
                button("Refresh").on_press_maybe(can_request.then_some(Message::Discover)),
                models,
                button("Test model").on_press_maybe(
                    (can_request && self.model.is_some()).then_some(Message::TestModel)
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text("Use a model service already running on this computer.").size(13),
        ]
        .spacing(10)
        .width(Length::Fill)
        .into()
    }

    fn endpoint_field(&self) -> Element<'_, Message> {
        field(
            "Local endpoint",
            text_input("Numeric loopback address and port", &self.endpoint)
                .on_input(Message::Endpoint),
        )
    }

    fn knowledge_view<'a>(&'a self, state: &'a State, can_request: bool) -> Element<'a, Message> {
        let selected = self.selected.and_then(|index| state.stores.get(index));
        let stores =
            state
                .stores
                .iter()
                .enumerate()
                .fold(column![].spacing(4), |rows, (index, store)| {
                    rows.push(
                        button(text(store.name.as_str()).wrapping(text::Wrapping::WordOrGlyph))
                            .width(Length::Fill)
                            .style(if self.selected == Some(index) {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(Message::SelectStore(index)),
                    )
                });
        let sidebar = column![
            text_input("New store name", &self.store_name)
                .on_input(Message::StoreName)
                .on_submit(Message::CreateStore),
            button("Create store").on_press(Message::CreateStore),
            stores,
        ]
        .spacing(8)
        .width(180);
        let mut details = column![].spacing(10).width(Length::Fill);
        if let Some(store) = selected {
            let sources = store.sources.iter().enumerate().fold(
                column![].spacing(6),
                |rows, (index, source)| {
                    rows.push(
                        row![
                            text(source.display().to_string())
                                .wrapping(text::Wrapping::WordOrGlyph)
                                .width(Length::Fill),
                            button("Remove").on_press(Message::RemoveSource(index))
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    )
                },
            );
            details = details.push(text(store.name.as_str()).size(19))
                .push(row![
                    text_input("Absolute path to a UTF-8 text file", &self.source).on_input(Message::Source).on_submit(Message::AddSource),
                    button("Add file").on_press(Message::AddSource),
                ].spacing(8))
                .push(sources)
                .push(self.endpoint_field())
                .push(field("Embedding model", text_input("Installed embedding model", &state.embedding).on_input(Message::Embedding)))
                .push(checkbox(self.consent)
                    .label("Allow these source excerpts and searches to be sent to this local service")
                    .on_toggle(Message::Consent))
                .push(text("Use a local embedding model you trust. The selected service can forward requests.").size(13))
                .push(button("Update index").on_press_maybe(
                    (can_request && self.consent && !store.sources.is_empty() && !state.embedding.trim().is_empty()).then_some(Message::Index),
                ))
                .push(row![
                    text_input("Test search", &self.query).on_input(Message::Query),
                    button("Search").on_press_maybe(
                        (can_request && self.consent && self.index.is_some() && !self.query.trim().is_empty()).then_some(Message::Search),
                    )
                ].spacing(8));
            for hit in &self.hits {
                details = details.push(text(hit.as_str()).wrapping(text::Wrapping::WordOrGlyph));
            }
            if self.confirm_remove {
                details = details.push(
                    row![
                        text("Remove this store from the profile? The source files stay on disk."),
                        button("Remove store")
                            .style(button::danger)
                            .on_press(Message::RemoveStore),
                        button("Cancel").on_press(Message::ConfirmRemove(false)),
                    ]
                    .spacing(8)
                    .wrap(),
                );
            } else {
                details =
                    details.push(button("Remove store…").on_press(Message::ConfirmRemove(true)));
            }
        } else {
            details = details.push(text("Select or create a knowledge store."));
        }
        column![
            text("Knowledge Stores").size(20),
            row![sidebar, details].spacing(16)
        ]
        .spacing(12)
        .width(Length::Fill)
        .into()
    }
}

fn preferences(state: &State, profiles: &[(u64, String)]) -> Element<'static, Message> {
    let options: Vec<_> = std::iter::once(Choice(None, "Use default".into()))
        .chain(
            profiles
                .iter()
                .map(|(id, name)| Choice(Some(*id), name.clone())),
        )
        .collect();
    let mut content = column![
        text("Local Preferences").size(20),
        text("Profile roles").size(17)
    ]
    .spacing(10)
    .width(Length::Fill);
    for role in Role::ALL {
        let selected = options
            .iter()
            .find(|option| option.0 == state.profile(role))
            .cloned()
            .or_else(|| options.first().cloned());
        content = content.push(field(
            role.label(),
            pick_list(options.clone(), selected, move |choice| {
                Message::Role(role, choice.0)
            })
            .width(Length::Fill),
        ));
    }
    content = content.push(field(
        "Embedding model",
        text_input("Installed embedding model", &state.embedding).on_input(Message::Embedding),
    ));
    content.into()
}

fn field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).width(175), control.into()]
        .align_y(Alignment::Center)
        .spacing(8)
        .into()
}

fn limited(value: &str, bytes: usize) -> &str {
    &value[..value.floor_char_boundary(bytes.min(value.len()))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_ignores_hits_for_an_earlier_query() {
        let mut dialog = Dialog::default();
        let mut state = State::default();
        let _ = dialog.update(Message::Query("new query".into()), &mut state, true);
        let _ = dialog.update(
            Message::Finished(
                0,
                Ok(Output::Hits {
                    query: "old query".into(),
                    hits: vec!["old result".into()],
                }),
            ),
            &mut state,
            true,
        );
        assert!(dialog.hits.is_empty());
    }

    #[test]
    fn knowledge_consent_cannot_survive_an_endpoint_or_embedding_model_change() {
        let mut dialog = Dialog::default();
        let mut state = State::default();
        let _ = dialog.update(Message::Consent(true), &mut state, true);
        let _ = dialog.update(Message::Endpoint("127.0.0.1:8080".into()), &mut state, true);
        assert!(!dialog.consent);
        let _ = dialog.update(Message::Consent(true), &mut state, true);
        let _ = dialog.update(Message::Embedding("embed-model".into()), &mut state, true);
        assert!(!dialog.consent);
    }
}
