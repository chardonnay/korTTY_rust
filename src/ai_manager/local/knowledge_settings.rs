//! A cancellable editor for one knowledge store in the AI Manager draft.

use std::path::Path;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Element, Length, Task};

use crate::ai_manager::search::{KnowledgeSetting, Reveal, Setting};
use crate::connection_window::CredentialChoice;
use crate::vault_window::{Kind as VaultKind, VaultWindow};
use crate::workspace::dialog_design;

use super::{
    Catalog, Dialog, EmbeddingField, EmbeddingScope, KnowledgeSourcePicker, Message, Role,
    SourceKind, State, Store, embedding_picker, knowledge_backend, knowledge_options, limited,
    local_ai,
};

#[derive(Clone)]
pub(crate) enum Action {
    Open { create: bool },
    Name(String),
    Advanced,
    InitialSourceKind(SourceKind),
    Apply,
    Cancel,
}

pub(super) struct Draft {
    original: Option<(usize, Store)>,
    store: Store,
    backend: knowledge_backend::Editor,
    advanced: bool,
    problem: Option<String>,
    dimensions_edited: bool,
    dimensions_model: Option<String>,
}

impl Draft {
    fn index(&self, state: &State) -> usize {
        self.original
            .as_ref()
            .map_or(state.stores.len(), |(index, _)| *index)
    }

    fn can_rename(&self) -> bool {
        self.store.id().is_some() || knowledge_backend::editable(&self.store)
    }

    fn prepare(&self, state: &State, vault: Option<&VaultWindow>) -> Result<Store, String> {
        let mut store = self.store.clone();
        store.name = store.name.trim().into();
        let index = self.index(state);
        if let Some((_, original)) = &self.original {
            if state.stores.get(index) != Some(original) {
                return Err("The store changed while the dialog was open. Reopen its settings to edit the latest draft.".into());
            }
            if store.embedding != original.embedding
                || store.qdrant != original.qdrant
                || store.local_dimensions != original.local_dimensions
            {
                store.automatic_embedding = None;
                store.automatic_policy_model = None;
                store.automatic_qdrant = None;
                store.automatic_qdrant_expanded = false;
            }
        } else {
            state.new_store_index(&store)?;
        }
        let vault_id = |store: &Store| {
            store
                .qdrant
                .as_ref()
                .and_then(|settings| settings.vault_key.clone())
        };
        let prior_id = self
            .original
            .as_ref()
            .and_then(|(_, store)| vault_id(store));
        if let Some(id) = vault_id(&store).filter(|id| Some(id) != prior_id.as_ref())
            && !qdrant_key_available(vault, &id)
        {
            return Err("Unlock the vault and choose a Qdrant API key.".into());
        }
        if state
            .stores
            .iter()
            .enumerate()
            .any(|(other, existing)| other != index && existing.name == store.name)
        {
            return Err("Enter a unique knowledge store name.".into());
        }
        store.validate()?;
        Ok(store)
    }
}

impl Dialog {
    fn source_advanced_view<'a>(
        &'a self,
        path: &Path,
        body: Element<'a, Message>,
        apply: Element<'a, Message>,
        cancel: Element<'a, Message>,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        column![
            container(
                text(path.display().to_string())
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph)
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(dialog_design::header),
            scrollable(container(body).padding([8, 14]).width(Length::Fill))
                .id(reveal.viewport.clone())
                .height(Length::Fill),
            self.status.as_deref().map(|message| {
                container(
                    text(message)
                        .size(13)
                        .style(text::danger)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .padding([0, 14])
            }),
            container(row![space::horizontal(), apply, cancel].spacing(8)).padding([8, 14]),
        ]
        .height(Length::Fill)
        .into()
    }
    pub(in crate::ai_manager) fn knowledge_source_updates_open(&self) -> bool {
        self.source_updates.is_some()
    }

    pub(in crate::ai_manager) fn knowledge_file_review_open(&self) -> bool {
        matches!(
            self.source_picker,
            Some(KnowledgeSourcePicker::ReviewFiles { .. })
        )
    }

    pub(in crate::ai_manager) fn knowledge_settings_title(&self) -> Option<&'static str> {
        if self.knowledge_source_advanced_open() {
            return Some("Advanced source settings");
        }
        if self.source_sync_editor.is_some() {
            return Some("Sync mode");
        }
        if self.source_removal.is_some() {
            return Some("Remove knowledge sources");
        }
        if self.knowledge_file_review_open() {
            return Some("Review files");
        }
        if self.source_updates.is_some() {
            return Some("Review source updates");
        }
        if matches!(
            self.source_picker,
            Some(KnowledgeSourcePicker::ChooseInitial(_))
        ) {
            return Some("Create knowledge store");
        }
        let draft = self.knowledge_settings.as_ref()?;
        Some(if draft.original.is_some() {
            "Configure knowledge store"
        } else {
            "Create knowledge store"
        })
    }

    pub(in crate::ai_manager) fn knowledge_source_sync_open(&self) -> bool {
        self.source_sync_editor.is_some()
    }

    pub(in crate::ai_manager) fn knowledge_source_advanced_open(&self) -> bool {
        self.file_size.is_some() || self.folder_advanced.is_some()
    }

    pub(in crate::ai_manager) fn knowledge_settings_height(&self) -> Option<f32> {
        if self.folder_advanced.is_some() {
            return Some(485.0);
        }
        if self.file_size.is_some() {
            return Some(228.0);
        }
        if self.source_sync_editor.is_some() {
            return Some(172.0);
        }
        if let Some(height) = self.source_removal_height() {
            return Some(height);
        }
        if self.knowledge_file_review_open() || self.source_updates.is_some() {
            return Some(550.0);
        }
        if matches!(
            self.source_picker,
            Some(KnowledgeSourcePicker::ChooseInitial(_))
        ) {
            return Some(178.0);
        }
        self.knowledge_settings
            .as_ref()
            .map(|draft| if draft.advanced { 470.0 } else { 278.0 })
    }

    pub(in crate::ai_manager) fn open_knowledge_settings(
        &mut self,
        state: &State,
        create: bool,
        advanced: bool,
    ) {
        if self.busy() || self.knowledge_source_picker_open() || self.knowledge_settings.is_some() {
            return;
        }
        let (original, store) = if create {
            let mut store = match Store::new("") {
                Ok(store) => store,
                Err(error) => {
                    self.status = Some(format!("Could not create the store ({error})."));
                    return;
                }
            };
            store.embedding = Some(state.default_embedding());
            (None, store)
        } else {
            let Some((index, store)) = self
                .selected
                .and_then(|index| state.stores.get(index).map(|store| (index, store)))
            else {
                return;
            };
            (Some((index, store.clone())), store.clone())
        };
        self.knowledge_settings = Some(Draft {
            advanced: advanced || store.qdrant.is_some(),
            original,
            store,
            backend: knowledge_backend::Editor::default(),
            problem: None,
            dimensions_edited: false,
            dimensions_model: None,
        });
    }

    pub(in crate::ai_manager) fn suggest_local_dimensions(&mut self) -> Task<Message> {
        let Some(draft) = &mut self.knowledge_settings else {
            return Task::none();
        };
        if draft.original.is_some() || draft.dimensions_edited {
            return Task::none();
        }
        let model = draft
            .store
            .embedding
            .as_ref()
            .and_then(|embedding| embedding.managed_model.clone());
        if model == draft.dimensions_model {
            return Task::none();
        }
        draft.dimensions_model = model.clone();
        draft.store.local_dimensions = None;
        let Some((store, model)) = draft.store.id().map(str::to_owned).zip(model) else {
            return Task::none();
        };
        let registry = local_ai::managed_models::Registry::new(self.profile_dir.clone());
        let worker_model = model.clone();
        let (send, receive) = async_channel::bounded(1);
        if std::thread::Builder::new()
            .name("knowledge-embedding-metadata".into())
            .spawn(move || {
                let value = registry.embedding_dimensions(&worker_model).ok().flatten();
                let _ = send.try_send(value);
            })
            .is_err()
        {
            return Task::none();
        }
        Task::perform(
            async move { receive.recv().await.ok().flatten() },
            move |value| Message::LocalDimensionsDetected {
                store: store.clone(),
                model: model.clone(),
                value,
            },
        )
    }

    pub(super) fn accept_local_dimensions(
        &mut self,
        store: &str,
        model: &str,
        value: Option<usize>,
    ) {
        if let Some(draft) = &mut self.knowledge_settings
            && draft.original.is_none()
            && !draft.dimensions_edited
            && draft.store.id() == Some(store)
            && draft.dimensions_model.as_deref() == Some(model)
        {
            draft.store.local_dimensions = value;
        }
    }

    pub(in crate::ai_manager) fn close_knowledge_settings(&mut self) {
        self.file_size = None;
        self.folder_advanced = None;
        self.source_sync_editor = None;
        self.knowledge_settings = None;
        self.source_removal = None;
        self.source_updates = None;
        if matches!(self.request, Some(super::Request::SourceUpdates { .. })) {
            self.cancel();
        }
        if matches!(
            self.source_picker,
            Some(
                KnowledgeSourcePicker::ChooseInitial(_) | KnowledgeSourcePicker::ReviewFiles { .. }
            )
        ) {
            self.close_knowledge_source_picker();
        }
    }

    pub(super) fn update_knowledge_settings(
        &mut self,
        action: Action,
        state: &mut State,
        vault: Option<&VaultWindow>,
    ) {
        match action {
            Action::Open { create } => self.open_knowledge_settings(state, create, false),
            Action::Cancel => self.close_knowledge_settings(),
            Action::Name(name) => {
                if let Some(draft) = &mut self.knowledge_settings
                    && draft.can_rename()
                {
                    draft.store.name = limited(&name, 128).into();
                    draft.problem = None;
                }
            }
            Action::Advanced => {
                if let Some(draft) = &mut self.knowledge_settings {
                    draft.advanced = !draft.advanced;
                }
            }
            Action::InitialSourceKind(kind) => {
                if let Some(KnowledgeSourcePicker::ChooseInitial(choice)) = &mut self.source_picker
                {
                    *choice = kind;
                }
            }
            Action::Apply if !self.busy() => {
                if let Some(draft) = &mut self.knowledge_settings {
                    match draft.prepare(state, vault) {
                        Ok(store) => {
                            if let Some((index, _)) = draft.original.as_ref() {
                                let index = *index;
                                state.stores[index] = store;
                                self.select_store(Some(index));
                                self.discard_stale_drafts(state);
                                self.status = Some("Knowledge store updated in the draft. Save AI Manager to apply it.".into());
                            } else {
                                self.close_knowledge_settings();
                                self.new_store = Some(store);
                                self.source_picker =
                                    Some(KnowledgeSourcePicker::ChooseInitial(SourceKind::Folder));
                                self.status = None;
                            }
                        }
                        Err(error) => draft.problem = Some(error),
                    }
                }
            }
            Action::Apply => {}
        }
    }

    pub(super) fn edit_knowledge_settings(
        &mut self,
        message: &Message,
        state: &State,
        vault: Option<&VaultWindow>,
    ) -> bool {
        let available = match message {
            Message::EmbeddingSource(EmbeddingScope::Store, id) => {
                id.as_deref().is_none_or(|id| self.embedding_available(id))
            }
            _ => true,
        };
        let Some(draft) = &mut self.knowledge_settings else {
            return false;
        };
        let index = draft.index(state);
        let editable = knowledge_backend::editable(&draft.store);
        match message {
            Message::ConfigureStore(field, value) => {
                if editable {
                    let embedding = draft
                        .store
                        .embedding
                        .get_or_insert_with(|| state.default_embedding());
                    match field {
                        EmbeddingField::Endpoint => embedding.endpoint = limited(value, 128).into(),
                        EmbeddingField::Model => {
                            embedding.model = limited(value, local_ai::MAX_MODEL_BYTES).into()
                        }
                    }
                }
            }
            Message::EmbeddingSource(EmbeddingScope::Store, id) => {
                if editable && available {
                    draft
                        .store
                        .embedding
                        .get_or_insert_with(|| state.default_embedding())
                        .managed_model = id.clone();
                }
            }
            Message::KnowledgeBackend { store, edit } => {
                if *store == index {
                    if let knowledge_backend::Edit::VaultKey(Some(id)) = edit
                        && !qdrant_key_available(vault, id)
                    {
                        draft.problem =
                            Some("Unlock the vault and choose a Qdrant API key.".into());
                        return true;
                    }
                    if editable
                        && draft.store.qdrant.is_none()
                        && matches!(edit, knowledge_backend::Edit::Dimensions(_))
                    {
                        draft.dimensions_edited = true;
                    }
                    draft.backend.apply(&mut draft.store, edit.clone());
                }
            }
            Message::BindKnowledgeProfile {
                store,
                role,
                expected_role,
                binding,
            } => {
                if *store == index && state.profile(*role) == Some(expected_role) {
                    match role {
                        Role::Text => draft.store.text_profile = binding.clone(),
                        Role::Coding => draft.store.coding_profile = binding.clone(),
                        _ => {}
                    }
                }
            }
            Message::EnableAutonomousKnowledge { store, enabled } => {
                if *store == index {
                    draft.store.autonomous_enabled = *enabled;
                }
            }
            _ => return false,
        }
        draft.problem = None;
        true
    }

    pub(in crate::ai_manager) fn knowledge_settings_view<'a>(
        &'a self,
        state: &'a State,
        profiles: &Catalog,
        vault_keys: Vec<CredentialChoice>,
        can_edit: bool,
        reveal: &Reveal,
    ) -> Option<Element<'a, Message>> {
        use KnowledgeSetting::*;
        if let Some(advanced) = &self.folder_advanced {
            let body = advanced
                .draft
                .scan_options_view(can_edit, reveal, false)
                .map(Message::EditFolderAdvanced);
            return Some(
                self.source_advanced_view(
                    &advanced.path,
                    body,
                    button(text("Review files…").size(13))
                        .padding([7, 22])
                        .on_press_maybe(can_edit.then_some(Message::ReviewFolderAdvanced))
                        .into(),
                    dialog_design::secondary_action("Cancel")
                        .on_press(Message::CancelFolderAdvanced)
                        .into(),
                    reveal,
                ),
            );
        }
        if let Some(edit) = &self.file_size
            && let Some((index, store)) = self
                .selected
                .and_then(|index| state.stores.get(index).map(|store| (index, store)))
            && store.sources.contains(&edit.path)
        {
            let path = &edit.path;
            let body = column![
                text(knowledge_options::file_size_help(path)).size(13),
                knowledge_options::size_spinner(
                    FileSize,
                    &edit.input,
                    can_edit,
                    Message::FileSize,
                    reveal
                ),
                (!edit.edited
                    && store
                        .file_size_limit(path)
                        .is_some_and(|limit| limit > store.effective_file_size_limit(path)))
                .then(|| text("Re-enter the saved value to review a higher limit.").size(13)),
            ]
            .spacing(8);
            return Some(
                self.source_advanced_view(
                    path,
                    body.into(),
                    reveal.action(
                        Setting::Knowledge(ApplyFileSize),
                        button(text("Apply size limit").size(13))
                            .padding([7, 22])
                            .on_press_maybe(
                                can_edit.then(|| Message::ApplyFileSize(index, path.clone())),
                            ),
                    ),
                    reveal.action(
                        Setting::Knowledge(CancelFileSize),
                        dialog_design::secondary_action("Cancel").on_press(Message::CancelFileSize),
                    ),
                    reveal,
                ),
            );
        }
        if let Some(view) = self.source_sync_view(can_edit) {
            return Some(view);
        }
        if let Some(view) = self.source_removal_review(can_edit) {
            return Some(view);
        }
        if let Some(view) = self.source_updates_view(can_edit) {
            return Some(view);
        }
        if let Some(KnowledgeSourcePicker::ReviewFiles {
            review,
            page,
            size_limit,
        }) = &self.source_picker
        {
            return Some(self.file_review_view(review, *page, size_limit, can_edit));
        }
        if let Some(KnowledgeSourcePicker::ChooseInitial(kind)) = self.source_picker {
            return Some(
                column![
                    container(
                        text("Add individual files or one recursively scanned folder.").size(15)
                    )
                    .padding([12, 14])
                    .width(Length::Fill)
                    .style(dialog_design::header),
                    container(
                        pick_list([SourceKind::File, SourceKind::Folder], Some(kind), |kind| {
                            Message::KnowledgeSettings(Action::InitialSourceKind(kind))
                        })
                        .width(Length::Fill)
                    )
                    .padding([12, 14]),
                    space::vertical(),
                    container(
                        row![
                            space::horizontal(),
                            button(text("Continue").size(13))
                                .padding([7, 22])
                                .on_press_maybe(
                                    can_edit.then_some(Message::PickNativeKnowledgeSources(kind))
                                ),
                            dialog_design::secondary_action("Cancel")
                                .on_press(Message::CloseKnowledgeSourcePicker),
                        ]
                        .spacing(8)
                    )
                    .padding([8, 14]),
                ]
                .height(Length::Fill)
                .into(),
            );
        }
        let draft = self.knowledge_settings.as_ref()?;
        let store = &draft.store;
        let index = draft.index(state);
        let can_embed = can_edit && knowledge_backend::editable(store);
        let can_rename = can_edit && draft.can_rename();
        let (endpoint, model, managed) = state.embedding_fields(store);
        let action = Message::KnowledgeSettings;
        let name = reveal.knowledge_input(
            Name,
            can_rename,
            text_input("Knowledge store name", &store.name)
                .style(dialog_design::form_input)
                .on_input_maybe(can_rename.then_some(move |name| action(Action::Name(name)))),
        );
        let bind_role =
            |role, setting, label: &'static str, current: Option<&super::ProfileRef>| {
                let expected = state.profile(role);
                let profile =
                    expected.and_then(|reference| profiles.resolve_profile_reference(reference));
                let binding = profile.and_then(|id| profiles.profile_reference(id));
                let selected = profile.is_some()
                    && current.and_then(|reference| profiles.resolve_profile_reference(reference))
                        == profile;
                let on_toggle = expected.cloned().zip(binding).filter(|_| can_edit).map(
                    |(expected_role, binding)| {
                        move |enabled: bool| Message::BindKnowledgeProfile {
                            store: index,
                            role,
                            expected_role: expected_role.clone(),
                            binding: enabled.then(|| binding.clone()),
                        }
                    },
                );
                reveal.action(
                    Setting::Knowledge(setting),
                    checkbox(selected).label(label).on_toggle_maybe(on_toggle),
                )
            };
        let roles = row![
            bind_role(
                Role::Text,
                TerminalChatProfile,
                "Text",
                store.text_profile.as_ref()
            ),
            bind_role(
                Role::Coding,
                CodeAnalysisProfile,
                "Coding",
                store.coding_profile.as_ref()
            ),
            reveal.action(
                Setting::Knowledge(AgentKnowledge),
                checkbox(store.autonomous_enabled)
                    .label("Agents and scheduled AI")
                    .on_toggle_maybe(can_edit.then_some(move |enabled| {
                        Message::EnableAutonomousKnowledge {
                            store: index,
                            enabled,
                        }
                    }))
            ),
        ]
        .spacing(10)
        .wrap();
        let form = column![
            settings_field("Name", name),
            reveal.wrap(Setting::Knowledge(EmbeddingSource), settings_field("Embedding model", embedding_picker(
                self.embedding_options(), managed, EmbeddingScope::Store, can_embed,
            ))),
            settings_field("Used for", roles),
            (!knowledge_backend::editable(store)).then(|| text("Remove the store's sources before changing its embedding model or vector storage.").size(13)),
            button(text(if draft.advanced { "▾ Advanced" } else { "▸ Advanced" }).size(13))
                .style(dialog_design::secondary_button)
                .width(Length::Fill)
                .padding([7, 8])
                .on_press(action(Action::Advanced)),
            draft.advanced.then(|| draft.backend.view(index, store, can_edit, vault_keys, reveal)),
            (draft.advanced && managed.is_none()).then(|| column![
                settings_field("Local model endpoint", reveal.knowledge_input(EmbeddingEndpoint, can_embed,
                    text_input("Numeric loopback address and port", endpoint)
                        .on_input_maybe(can_embed.then_some(|value| Message::ConfigureStore(EmbeddingField::Endpoint, value))))),
                settings_field("Local model name", reveal.knowledge_input(EmbeddingModel, can_embed,
                    text_input("Installed embedding model", model)
                        .on_input_maybe(can_embed.then_some(|value| Message::ConfigureStore(EmbeddingField::Model, value))))),
            ].spacing(8)),
        ].spacing(9);
        Some(
            column![
                container(
                    text("Name the store, choose its embedding model, and assign its roles.")
                        .size(15)
                )
                .padding([10, 14])
                .width(Length::Fill)
                .style(dialog_design::header),
                scrollable(container(form).padding(14))
                    .id(reveal.viewport.clone())
                    .height(Length::Fill),
                draft
                    .problem
                    .as_deref()
                    .map(|error| container(text(error).size(13)).padding([0, 14])),
                container(
                    row![
                        space::horizontal(),
                        button(
                            text(if draft.original.is_some() {
                                "Save"
                            } else {
                                "Next"
                            })
                            .size(13)
                        )
                        .padding([7, 22])
                        .on_press_maybe(
                            (can_edit && !store.name.trim().is_empty())
                                .then_some(action(Action::Apply))
                        ),
                        dialog_design::secondary_action("Cancel").on_press(action(Action::Cancel)),
                    ]
                    .spacing(8)
                )
                .padding(14),
            ]
            .height(Length::Fill)
            .into(),
        )
    }
}

pub(super) fn settings_field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(13).width(120), control.into()]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into()
}

fn qdrant_key_available(vault: Option<&VaultWindow>, id: &str) -> bool {
    vault.is_some_and(|vault| {
        vault
            .choices(VaultKind::QdrantApiKey)
            .iter()
            .any(|choice| choice.id == id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_cancel_and_apply_are_scoped_to_one_store() {
        let fixture = crate::snippets::tests::Profile::new();
        let source = fixture.0.canonicalize().unwrap().join("notes.txt");
        let mut existing = Store::new("Existing store").unwrap();
        existing.add_source(source.clone()).unwrap();
        let saved = State {
            stores: vec![existing.clone()],
            ..State::default()
        };
        let mut state = saved.clone();
        let mut dialog = Dialog::new(fixture.0.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut state, &saved, true);
        let send = |dialog: &mut Dialog, state: &mut State, action| {
            let _ = dialog.update(Message::KnowledgeSettings(action), state, &saved, true);
        };

        send(&mut dialog, &mut state, Action::Open { create: false });
        send(
            &mut dialog,
            &mut state,
            Action::Name("Discarded name".into()),
        );
        assert_eq!(state, saved);
        send(&mut dialog, &mut state, Action::Cancel);
        assert_eq!(state, saved);

        send(&mut dialog, &mut state, Action::Open { create: false });
        let _ = dialog.update(
            Message::ConfigureStore(EmbeddingField::Model, "other-model".into()),
            &mut state,
            &saved,
            true,
        );
        let _ = dialog.update(
            Message::KnowledgeBackend {
                store: 0,
                edit: knowledge_backend::Edit::Backend(knowledge_backend::Backend::Qdrant),
            },
            &mut state,
            &saved,
            true,
        );
        send(
            &mut dialog,
            &mut state,
            Action::Name("Renamed store".into()),
        );
        send(&mut dialog, &mut state, Action::Apply);
        assert!(dialog.knowledge_settings_title().is_none());
        assert_eq!(state.stores[0].name, "Renamed store");
        assert_eq!(state.stores[0].id(), existing.id());
        assert_eq!(state.stores[0].sources, [source]);
        assert_eq!(state.stores[0].qdrant, existing.qdrant);
        assert_eq!(state.stores[0].embedding, existing.embedding);

        send(&mut dialog, &mut state, Action::Open { create: false });
        state.stores[0].name = "Concurrent draft edit".into();
        send(&mut dialog, &mut state, Action::Name("Stale rename".into()));
        send(&mut dialog, &mut state, Action::Apply);
        assert_eq!(state.stores[0].name, "Concurrent draft edit");
        assert!(
            dialog
                .knowledge_settings
                .as_ref()
                .unwrap()
                .problem
                .as_deref()
                .unwrap()
                .contains("changed")
        );
        send(&mut dialog, &mut state, Action::Cancel);

        send(&mut dialog, &mut state, Action::Open { create: true });
        send(&mut dialog, &mut state, Action::Name("New store".into()));
        assert_eq!(state.stores.len(), 1);
        send(&mut dialog, &mut state, Action::Apply);
        assert_eq!(state.stores.len(), 1);
        assert_eq!(
            dialog.knowledge_settings_title(),
            Some("Create knowledge store")
        );
        assert!(matches!(
            dialog.source_picker,
            Some(KnowledgeSourcePicker::ChooseInitial(SourceKind::Folder))
        ));
        dialog.close_knowledge_settings();
        assert!(dialog.new_store.is_none());
        assert_eq!(dialog.selected, Some(0));
    }
}
