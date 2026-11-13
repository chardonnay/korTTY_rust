//! Review external skill documents before changing the library draft.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Font, Length, Task};

use crate::ai_manager::profiles::picker::DismissOnEscape as _;

use super::super::external::{
    self as remote, Candidate, Client, Decision, Document, ProviderProfile, Update, UpdateClient,
};
use super::{Editor, Skill, Store, bounded};

mod providers;

const MAX_QUERY_BYTES: usize = 2048;
static WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub(crate) enum Message {
    OpenImport,
    OpenUpdates,
    OpenProviders,
    Close,
    Provider(u64),
    Query(String),
    Search,
    Preview(u64, usize),
    Queue(u64, usize),
    Unqueue(u64, usize),
    Import(u64),
    Check,
    ShowUpdate(u64, usize),
    Decide(u64, usize, Decision),
    Back,
    Providers(providers::Message),
    Completed(u64, Result<Completion, String>),
}

#[derive(Clone)]
pub(crate) enum Completion {
    Search(Arc<Vec<Candidate>>),
    Preview(usize, Arc<Document>),
    Updates(Arc<Vec<Update>>, String),
    Tested,
}

pub(super) struct Panel {
    screen: Screen,
    pending: Option<Pending>,
    notice: Option<String>,
}

enum Screen {
    Import(Catalog),
    Updates(Updates),
    Providers(providers::Panel),
}

#[derive(Default)]
struct Catalog {
    provider: Option<u64>,
    query: String,
    generation: u64,
    profiles: Vec<ProviderProfile>,
    results: Option<Arc<Vec<Candidate>>>,
    preview: Option<(usize, Arc<Document>)>,
    queued: BTreeMap<usize, Arc<Document>>,
}

#[derive(Default)]
struct Updates {
    generation: u64,
    profiles: Vec<ProviderProfile>,
    found: Option<Arc<Vec<Update>>>,
    resolved: Vec<usize>,
    review: Option<usize>,
}

struct Pending {
    ticket: u64,
    profiles: Vec<ProviderProfile>,
    cancelled: Arc<AtomicBool>,
    _cancel_request: Option<remote::Cancellation>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Worker;

impl Drop for Worker {
    fn drop(&mut self) {
        WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

impl Panel {
    fn new(screen: Screen) -> Self {
        Self {
            screen,
            pending: None,
            notice: None,
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn can_check(store: &Store) -> bool {
        store.skills.values().any(|skill| skill.external.is_some())
    }

    pub(super) fn pending_edit(&self) -> bool {
        matches!(&self.screen, Screen::Providers(providers) if providers.pending_edit())
    }

    pub(super) fn commit(&mut self, store: &mut Store) -> Result<(), String> {
        if let Screen::Providers(providers) = &mut self.screen {
            providers.commit(store)?;
        }
        Ok(())
    }

    fn start(
        &mut self,
        profiles: Vec<ProviderProfile>,
        ticket: &mut u64,
        cancel_request: Option<remote::Cancellation>,
        work: impl FnOnce(&AtomicBool) -> Result<Completion, String> + Send + 'static,
    ) -> Task<Message> {
        if WORKER_ACTIVE.swap(true, Ordering::AcqRel) {
            self.notice =
                Some("An external skill request is still stopping. Try again shortly.".into());
            return Task::none();
        }
        let worker = Worker;
        let (reply, receive) = async_channel::bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        if std::thread::Builder::new()
            .name("kortty-external-skills".into())
            .spawn(move || {
                let result = work(&stop);
                drop(worker);
                let _ = reply.try_send(result);
            })
            .is_err()
        {
            self.notice = Some("Could not start the external skill worker.".into());
            return Task::none();
        }
        *ticket = ticket.wrapping_add(1);
        let ticket = *ticket;
        self.pending = Some(Pending {
            ticket,
            profiles,
            cancelled,
            _cancel_request: cancel_request,
        });
        self.notice = None;
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The external skill worker stopped.".into()));
            Message::Completed(ticket, result)
        })
    }

    fn complete(&mut self, store: &Store, ticket: u64, result: Result<Completion, String>) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.ticket == ticket)
        {
            return;
        }
        let pending = self.pending.take().expect("checked request");
        if pending.profiles != store.external_providers {
            self.notice = Some("The provider settings changed. Start a fresh review.".into());
            return;
        }
        match (&mut self.screen, result) {
            (_, Err(error)) => self.notice = Some(error),
            (Screen::Import(catalog), Ok(Completion::Search(results))) => {
                self.notice = Some(format!("{} skills found.", results.len()));
                catalog.results = Some(results);
                catalog.generation = ticket;
                catalog.profiles.clone_from(&pending.profiles);
            }
            (Screen::Import(catalog), Ok(Completion::Preview(index, document))) => {
                catalog.preview = Some((index, document));
            }
            (Screen::Updates(updates), Ok(Completion::Updates(found, report))) => {
                updates.found = Some(found);
                updates.generation = ticket;
                updates.profiles.clone_from(&pending.profiles);
                self.notice = Some(report);
            }
            (Screen::Providers(_), Ok(Completion::Tested)) => {
                self.notice = Some("The provider request succeeded.".into());
            }
            _ => {}
        }
    }

    fn search(&mut self, store: &Store, ticket: &mut u64) -> Task<Message> {
        let Screen::Import(catalog) = &mut self.screen else {
            return Task::none();
        };
        let Some(id) = catalog.provider else {
            return Task::none();
        };
        if !catalog.can_search(store) {
            return Task::none();
        }
        let query = catalog.query.trim().to_owned();
        catalog.clear_results();
        let profiles = store.external_providers.clone();
        let client = match Client::new(&profiles, id) {
            Ok(client) => client,
            Err(error) => {
                self.notice = Some(error);
                return Task::none();
            }
        };
        let cancel = client.cancel_handle();
        self.start(profiles, ticket, Some(cancel), move |_| {
            client
                .search(&query)
                .map(|results| Completion::Search(Arc::new(results)))
        })
    }

    fn preview(
        &mut self,
        store: &Store,
        ticket: &mut u64,
        generation: u64,
        index: usize,
    ) -> Task<Message> {
        let Screen::Import(catalog) = &mut self.screen else {
            return Task::none();
        };
        if !catalog.current(store, generation) {
            return Task::none();
        }
        if let Some(document) = catalog.queued.get(&index) {
            catalog.preview = Some((index, Arc::clone(document)));
            return Task::none();
        }
        let (Some(id), Some(results)) = (catalog.provider, catalog.results.clone()) else {
            return Task::none();
        };
        if index >= results.len() {
            return Task::none();
        }
        catalog.preview = None;
        let profiles = store.external_providers.clone();
        let client = match Client::new(&profiles, id) {
            Ok(client) => client,
            Err(error) => {
                self.notice = Some(error);
                return Task::none();
            }
        };
        let cancel = client.cancel_handle();
        self.start(profiles, ticket, Some(cancel), move |_| {
            client
                .fetch(&results[index])
                .map(|document| Completion::Preview(index, Arc::new(document)))
        })
    }

    fn check(&mut self, store: &Store, ticket: &mut u64) -> Task<Message> {
        let Screen::Updates(updates) = &mut self.screen else {
            return Task::none();
        };
        *updates = Updates::default();
        let sources: Vec<_> = store
            .skills
            .iter()
            .filter(|(_, skill)| skill.external.is_some())
            .map(|(&id, skill)| (id, skill.clone()))
            .collect();
        if sources.is_empty() {
            return Task::none();
        }
        let profiles = store.external_providers.clone();
        let client = match UpdateClient::new(&profiles) {
            Ok(client) => client,
            Err(error) => {
                self.notice = Some(error);
                return Task::none();
            }
        };
        let cancel = client.cancel_handle();
        self.start(profiles, ticket, Some(cancel), move |cancelled| {
            check_updates(&client, &sources, cancelled)
        })
    }

    fn import(&mut self, store: &mut Store, generation: u64) -> Result<u64, String> {
        let Screen::Import(catalog) = &self.screen else {
            return Err("Open the external skill import first.".into());
        };
        if !catalog.current(store, generation) || catalog.queued.is_empty() {
            return Err("Review the selected skills again before importing.".into());
        }
        let mut next = store.clone();
        let mut selected = None;
        for document in catalog.queued.values() {
            selected = Some(next.import_external(document)?);
        }
        *store = next;
        Ok(selected.expect("checked import selection"))
    }

    fn update_providers(
        &mut self,
        store: &mut Store,
        ticket: &mut u64,
        message: providers::Message,
    ) -> Task<Message> {
        let Screen::Providers(providers) = &mut self.screen else {
            return Task::none();
        };
        if matches!(message, providers::Message::Test) {
            let prepared = providers.test(store).and_then(|(profiles, id, sample)| {
                let client = Client::new(&profiles, id)?;
                Ok((client, sample))
            });
            let (client, sample) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.notice = Some(error);
                    return Task::none();
                }
            };
            let cancel = client.cancel_handle();
            return self.start(
                store.external_providers.clone(),
                ticket,
                Some(cancel),
                move |_| client.test(&sample).map(|()| Completion::Tested),
            );
        }
        self.notice = match providers.update(store, message) {
            Ok(notice) => notice.map(str::to_owned),
            Err(error) => Some(error),
        };
        Task::none()
    }

    pub(super) fn view<'a>(&'a self, store: &'a Store) -> Element<'a, Message> {
        let busy = self.busy();
        let can_switch = !busy && !self.pending_edit();
        let body = match &self.screen {
            Screen::Import(catalog) => catalog.view(store, busy),
            Screen::Updates(updates) => updates.view(store, busy),
            Screen::Providers(providers) => providers.view(store, busy).map(Message::Providers),
        };
        column![
            row![
                button("Browse external skills").on_press_maybe(
                    (can_switch && !matches!(self.screen, Screen::Import(_)))
                        .then_some(Message::OpenImport)
                ),
                button("Check for updates").on_press_maybe(
                    (can_switch
                        && !matches!(self.screen, Screen::Updates(_))
                        && Self::can_check(store))
                    .then_some(Message::OpenUpdates)
                ),
                button("Providers…").on_press_maybe(
                    (can_switch && !matches!(self.screen, Screen::Providers(_)))
                        .then_some(Message::OpenProviders)
                ),
            ]
            .spacing(8)
            .wrap(),
            body,
            self.notice
                .as_deref()
                .map(|notice| text(notice).size(13).wrapping(text::Wrapping::WordOrGlyph)),
            busy.then(|| text("Contacting the selected provider…").size(13)),
            button(if busy {
                "Cancel request"
            } else if self.pending_edit() {
                "Discard provider edits and close"
            } else {
                "Close"
            })
            .on_press(Message::Close),
        ]
        .height(Length::Fill)
        .spacing(8)
        .into()
    }
}

impl Editor {
    pub(super) fn update_external(
        &mut self,
        store: &mut Store,
        message: Message,
    ) -> Task<super::Message> {
        match message {
            Message::Close => {
                self.external = None;
                return Task::none();
            }
            Message::OpenImport | Message::OpenUpdates | Message::OpenProviders => {
                if self
                    .external
                    .as_ref()
                    .is_some_and(|panel| panel.busy() || panel.pending_edit())
                    || !self.commit_or_notice(store)
                {
                    return Task::none();
                }
                let screen = match message {
                    Message::OpenImport => Screen::Import(Catalog {
                        provider: store
                            .external_providers
                            .iter()
                            .find(|profile| profile.enabled)
                            .map(|profile| profile.id),
                        ..Catalog::default()
                    }),
                    Message::OpenUpdates => Screen::Updates(Updates::default()),
                    Message::OpenProviders => Screen::Providers(providers::Panel::new(store)),
                    _ => unreachable!(),
                };
                self.external = Some(Panel::new(screen));
                return Task::none();
            }
            _ => {}
        }
        let Some(panel) = self.external.as_mut() else {
            return Task::none();
        };
        if let Message::Completed(ticket, result) = message {
            panel.complete(store, ticket, result);
            return Task::none();
        }
        if panel.busy() {
            return Task::none();
        }

        let task = match message {
            Message::Search => panel.search(store, &mut self.ticket),
            Message::Preview(generation, index) => {
                panel.preview(store, &mut self.ticket, generation, index)
            }
            Message::Check => panel.check(store, &mut self.ticket),
            Message::Providers(message) => panel.update_providers(store, &mut self.ticket, message),
            Message::Import(generation) => {
                match panel.import(store, generation) {
                    Ok(id) => {
                        self.external = None;
                        self.search.clear();
                        self.select(store, id);
                        self.notice = Some(
                            "External skills imported disabled into the library draft.".into(),
                        );
                    }
                    Err(error) => panel.notice = Some(error),
                }
                Task::none()
            }
            other => {
                if let Some(id) = panel.edit(store, other) {
                    self.select(store, id);
                }
                Task::none()
            }
        };
        task.map(super::Message::External)
    }
}

impl Panel {
    fn edit(&mut self, store: &mut Store, message: Message) -> Option<u64> {
        match (&mut self.screen, message) {
            (Screen::Import(catalog), Message::Provider(id))
                if catalog.provider != Some(id)
                    && store
                        .external_providers
                        .iter()
                        .any(|profile| profile.id == id && profile.enabled) =>
            {
                catalog.provider = Some(id);
                catalog.clear_results();
                self.notice = None;
            }
            (Screen::Import(catalog), Message::Query(query)) if catalog.query != query => {
                catalog.query = bounded(query, MAX_QUERY_BYTES);
                catalog.clear_results();
                self.notice = None;
            }
            (Screen::Import(catalog), Message::Queue(generation, index))
                if catalog.current(store, generation) =>
            {
                if let Some((previewed, document)) = &catalog.preview
                    && *previewed == index
                {
                    catalog.queued.insert(index, Arc::clone(document));
                }
            }
            (Screen::Import(catalog), Message::Unqueue(generation, index))
                if catalog.current(store, generation) =>
            {
                catalog.queued.remove(&index);
            }
            (Screen::Import(catalog), Message::Back) => catalog.preview = None,
            (Screen::Updates(updates), Message::ShowUpdate(generation, index)) => {
                if updates.reviewed(store, generation, index).is_some() {
                    updates.review = Some(index);
                }
            }
            (Screen::Updates(updates), Message::Back) => updates.review = None,
            (Screen::Updates(updates), Message::Decide(generation, index, decision))
                if updates.review == Some(index) =>
            {
                let Some(update) = updates.reviewed(store, generation, index) else {
                    self.notice =
                        Some("The skill or its provider changed. Check for updates again.".into());
                    return None;
                };
                match store.apply_external_update(update, decision) {
                    Ok(id) => {
                        updates.resolved.push(index);
                        updates.review = None;
                        self.notice = Some(
                            match decision {
                                Decision::Replace => "The update was applied to the library draft.",
                                Decision::Copy => "A disabled copy was added to the library draft.",
                                Decision::Skip => {
                                    "The reviewed revision was skipped in the library draft."
                                }
                            }
                            .into(),
                        );
                        return Some(id);
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            _ => {}
        }
        None
    }
}

impl Catalog {
    fn can_search(&self, store: &Store) -> bool {
        store.external_providers.iter().any(|profile| {
            Some(profile.id) == self.provider
                && profile.enabled
                && (!self.query.trim().is_empty()
                    || profile.provider == remote::Provider::Http
                        && !profile.base_url.trim().is_empty())
        })
    }

    fn current(&self, store: &Store, generation: u64) -> bool {
        self.generation == generation && self.profiles == store.external_providers
    }

    fn clear_results(&mut self) {
        self.results = None;
        self.preview = None;
        self.queued.clear();
        self.profiles.clear();
    }

    fn view<'a>(&'a self, store: &'a Store, busy: bool) -> Element<'a, Message> {
        let choices: Vec<_> = store
            .external_providers
            .iter()
            .filter(|profile| profile.enabled)
            .map(|profile| Choice {
                id: profile.id,
                name: profile.name.clone(),
            })
            .collect();
        let has_choices = !choices.is_empty();
        let selected = choices
            .iter()
            .find(|choice| Some(choice.id) == self.provider)
            .cloned();
        let profile = selected.as_ref().and_then(|choice| {
            store
                .external_providers
                .iter()
                .find(|profile| profile.id == choice.id)
        });
        let can_search = !busy && self.can_search(store);
        let picker: Element<'_, Message> = if busy {
            text(profile.map_or("Provider", |profile| profile.name.as_str())).into()
        } else {
            pick_list(choices, selected, |choice| Message::Provider(choice.id))
                .placeholder("Choose a provider")
                .dismiss_on_escape()
        };
        let query = text_input(
            "Skill name, repository reference, or Markdown URL",
            &self.query,
        )
        .on_input_maybe((!busy).then_some(Message::Query))
        .on_submit_maybe(can_search.then_some(Message::Search));
        let generation = self.generation;
        let results = self.results.as_deref().map(|results| {
            let mut items = column![].spacing(5);
            for (index, candidate) in results.iter().enumerate() {
                let prefix = if self.queued.contains_key(&index) {
                    "Selected · "
                } else {
                    ""
                };
                let label = format!("{prefix}{}", candidate.name);
                items = items.push(
                    button(text(label).size(13).wrapping(text::Wrapping::WordOrGlyph))
                        .width(Length::Fill)
                        .style(
                            if self
                                .preview
                                .as_ref()
                                .is_some_and(|(selected, _)| *selected == index)
                            {
                                button::primary
                            } else {
                                button::text
                            },
                        )
                        .on_press_maybe((!busy).then_some(Message::Preview(generation, index))),
                );
            }
            scrollable(items).width(Length::Fill).height(Length::Fill)
        });
        let body: Element<'a, Message> = if let Some((index, document)) = &self.preview {
            let candidate = self
                .results
                .as_deref()
                .and_then(|results| results.get(*index));
            column![
                candidate.map(|candidate| candidate_metadata(candidate)),
                document_view(document),
                row![
                    if self.queued.contains_key(index) {
                        button("Remove from selection")
                            .on_press(Message::Unqueue(generation, *index))
                    } else {
                        button("Select reviewed skill").on_press(Message::Queue(generation, *index))
                    },
                    button("Back to results").on_press(Message::Back),
                ]
                .spacing(8)
                .wrap(),
            ]
            .spacing(8)
            .height(Length::Fill)
            .into()
        } else {
            container(text(if busy {
                "Loading the provider response…"
            } else {
                "Search a provider, then review each skill before adding it to the import selection."
            }).wrapping(text::Wrapping::WordOrGlyph))
                .height(Length::Fill)
                .into()
        };
        column![
            text("Import external skills").size(19),
            row![
                picker,
                query,
                button("Search").on_press_maybe(can_search.then_some(Message::Search)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap(),
            profile.map(|profile| text(provider_hint(profile))
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)),
            (!has_choices).then(|| text("Enable an external skill provider to search.")
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)),
            row![
                results.map(|list| container(list)
                    .width(Length::FillPortion(1))
                    .height(Length::Fill)),
                container(body)
                    .width(Length::FillPortion(2))
                    .height(Length::Fill),
            ]
            .spacing(12)
            .height(Length::Fill),
            row![
                text(format!("{} reviewed skills selected", self.queued.len())).size(13),
                button("Import selected disabled").on_press_maybe(
                    (!busy && self.current(store, generation) && !self.queued.is_empty())
                        .then_some(Message::Import(generation)),
                ),
            ]
            .align_y(Alignment::Center)
            .spacing(8)
            .wrap(),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

fn provider_hint(profile: &ProviderProfile) -> String {
    use remote::Provider;
    let hint = match profile.provider {
        Provider::GitHub => {
            "Enter a repository, owner/repository@skill, a GitHub URL, or keywords."
        }
        Provider::SkillsMp => "Enter keywords to search the SkillsMP catalogue.",
        Provider::Http => "Enter a Markdown URL or a path on the configured server.",
    };
    if profile.base_url.is_empty() {
        hint.into()
    } else {
        format!("{hint} Server: {}", profile.base_url)
    }
}

fn candidate_metadata(candidate: &Candidate) -> Element<'_, Message> {
    let mut metadata = column![text(&candidate.name).size(17)];
    for value in [
        &candidate.description,
        &candidate.author,
        &candidate.reference,
        &candidate.source_url,
    ] {
        if !value.is_empty() {
            metadata = metadata.push(text(value).size(13).wrapping(text::Wrapping::WordOrGlyph));
        }
    }
    if let Some(stars) = candidate.stars {
        metadata = metadata.push(text(format!("{stars} stars")).size(13));
    }
    metadata.spacing(3).into()
}

fn document_view(document: &Document) -> Element<'_, Message> {
    column![
        text(&document.skill.name)
            .size(16)
            .wrapping(text::Wrapping::WordOrGlyph),
        (!document.skill.description.is_empty()).then(|| text(&document.skill.description)
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph)),
        text(format!("Revision {}", document.revision))
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph),
        text(&document.source_url)
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph),
        scrollable(
            text(&document.skill.content)
                .font(Font::MONOSPACE)
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)
        )
        .height(Length::Fill),
    ]
    .spacing(5)
    .height(Length::Fill)
    .into()
}

impl Updates {
    fn reviewed(&self, store: &Store, generation: u64, index: usize) -> Option<&Update> {
        if self.generation != generation
            || self.profiles != store.external_providers
            || self.resolved.contains(&index)
        {
            return None;
        }
        let update = self.found.as_ref()?.get(index)?;
        (store.skills.get(&update.id) == Some(&update.original)).then_some(update)
    }

    fn view<'a>(&'a self, store: &'a Store, busy: bool) -> Element<'a, Message> {
        let generation = self.generation;
        let body: Element<'a, Message> = if let Some(index) = self.review
            && let Some(update) = self.reviewed(store, generation, index)
        {
            let before = column![
                text("Current version").size(15),
                text(&update.original.description)
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
                scrollable(
                    text(&update.original.content)
                        .font(Font::MONOSPACE)
                        .size(13)
                        .wrapping(text::Wrapping::WordOrGlyph)
                )
                .height(Length::Fill),
            ]
            .spacing(6)
            .height(Length::Fill)
            .width(Length::Fill);
            let after = column![
                text("Available update").size(15),
                document_view(&update.latest),
            ]
            .spacing(6)
            .height(Length::Fill)
            .width(Length::Fill);
            column![
                text(&update.original.name).size(17),
                (!update.can_replace).then(|| text("This skill has local edits. Keep them and import the update as a disabled copy, or skip this revision.").size(13).wrapping(text::Wrapping::WordOrGlyph)),
                row![before, after].spacing(12).height(Length::Fill),
                row![
                    button("Replace with update").on_press_maybe(update.can_replace.then_some(Message::Decide(generation, index, Decision::Replace))),
                    button("Save disabled copy").on_press(Message::Decide(generation, index, Decision::Copy)),
                    button("Skip revision").on_press(Message::Decide(generation, index, Decision::Skip)),
                    button("Back to updates").on_press(Message::Back),
                ].spacing(8).wrap(),
            ].spacing(8).height(Length::Fill).into()
        } else if let Some(found) = &self.found {
            let mut items = column![].spacing(6);
            for (index, update) in found
                .iter()
                .enumerate()
                .filter(|(index, _)| !self.resolved.contains(index))
            {
                items = items.push(
                    row![
                        text(&update.original.name)
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        button("Review update…").on_press_maybe(
                            self.reviewed(store, generation, index)
                                .map(|_| Message::ShowUpdate(generation, index))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .wrap(),
                );
            }
            if found.len() == self.resolved.len() {
                items = items.push(text("No unreviewed updates from this check.").size(13));
            }
            scrollable(items).height(Length::Fill).into()
        } else {
            container(text("Check the providers saved with imported skills. Review each new version before applying it.").wrapping(text::Wrapping::WordOrGlyph))
                .height(Length::Fill)
                .into()
        };
        column![
            text("External skill updates").size(19),
            body,
            button("Check external skills")
                .on_press_maybe((!busy && Panel::can_check(store)).then_some(Message::Check)),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

fn check_updates(
    client: &UpdateClient,
    sources: &[(u64, Skill)],
    cancelled: &AtomicBool,
) -> Result<Completion, String> {
    let mut updates = Vec::new();
    let mut first_failure = None;
    let mut failures = 0;
    let mut checked = 0;
    for (id, skill) in sources {
        if cancelled.load(Ordering::Acquire) {
            return Err("External skill update check cancelled.".into());
        }
        checked += 1;
        match client.check(*id, skill) {
            Ok(Some(update)) => updates.push(update),
            Ok(None) => {}
            Err(error) => {
                failures += 1;
                first_failure.get_or_insert(error);
            }
        }
    }
    let mut report = format!(
        "Checked {checked} of {} skills. {} updates found.",
        sources.len(),
        updates.len()
    );
    if let Some(error) = first_failure {
        report.push_str(&format!(" {failures} checks failed. {error}"));
    }
    Ok(Completion::Updates(Arc::new(updates), report))
}

pub(super) fn source_view(skill: &Skill) -> Option<Element<'_, super::Message>> {
    let source = skill.external.as_ref()?;
    Some(
        column![
            text(format!("External skill · revision {}", source.revision))
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph),
            text(&source.source_url)
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph),
            source
                .locally_modified(skill)
                .then(|| text("Locally modified").size(13)),
        ]
        .spacing(3)
        .into(),
    )
}

#[derive(Clone, Eq, PartialEq)]
struct Choice {
    id: u64,
    name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::skills::editor::Message as EditorMessage;

    #[test]
    fn reopened_catalogue_ignores_the_cancelled_request_and_changed_provider() {
        let mut editor = Editor::default();
        let mut store = Store::default();
        let pending = |ticket: u64, store: &Store| Pending {
            ticket,
            profiles: store.external_providers.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            _cancel_request: None,
        };
        let _ = editor.update(
            &mut store,
            EditorMessage::External(Message::OpenImport),
            None,
        );
        let first = pending(1, &store);
        let cancelled = Arc::clone(&first.cancelled);
        editor.external.as_mut().unwrap().pending = Some(first);
        assert!(editor.busy());
        let _ = editor.update(&mut store, EditorMessage::Cancel, None);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(!editor.file_prompt_open());

        let _ = editor.update(
            &mut store,
            EditorMessage::External(Message::OpenImport),
            None,
        );
        editor.external.as_mut().unwrap().pending = Some(pending(2, &store));
        let complete = |ticket| {
            EditorMessage::External(Message::Completed(
                ticket,
                Ok(Completion::Search(Arc::new(Vec::new()))),
            ))
        };
        let _ = editor.update(&mut store, complete(1), None);
        assert!(editor.busy());
        store.external_providers[0].name.push_str(" updated");
        let _ = editor.update(&mut store, complete(2), None);
        assert!(!editor.busy());
        let panel = editor.external.as_ref().unwrap();
        assert!(matches!(&panel.screen, Screen::Import(catalog) if catalog.results.is_none()));
        assert_eq!(
            panel.notice.as_deref(),
            Some("The provider settings changed. Start a fresh review.")
        );
    }

    #[test]
    fn provider_edits_stay_in_the_form_until_applied_to_the_library_draft() {
        let mut editor = Editor::default();
        let mut store = Store::default();
        let original = store.clone();
        let _ = editor.update(
            &mut store,
            EditorMessage::External(Message::OpenProviders),
            None,
        );
        let edit = |message| EditorMessage::External(Message::Providers(message));
        let _ = editor.update(
            &mut store,
            edit(providers::Message::Name("Private catalogue".into())),
            None,
        );
        assert!(editor.pending_edit());
        assert_eq!(store, original);
        let _ = editor.update(
            &mut store,
            EditorMessage::External(Message::OpenImport),
            None,
        );
        assert!(matches!(
            &editor.external.as_ref().unwrap().screen,
            Screen::Providers(_)
        ));
        let _ = editor.update(&mut store, edit(providers::Message::Apply), None);
        assert!(!editor.pending_edit());
        assert_eq!(store.external_providers[0].name, "Private catalogue");
    }
}
