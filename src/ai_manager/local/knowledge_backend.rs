//! Knowledge-store backend settings and transfer disclosures.

use std::fmt;

use iced::widget::{column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Length};

use crate::connection_window::CredentialChoice;

use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{KnowledgeSetting, Reveal, Setting};
use crate::workspace::dialog_design::{self, number_step};

use super::Message;
use super::knowledge::qdrant::{self, Settings};
use super::knowledge::{MAX_DIMENSIONS, Store};

const SPINNER_WIDTH: f32 = 156.0;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Backend {
    Local,
    Qdrant,
}

impl Backend {
    const ALL: [Self; 2] = [Self::Local, Self::Qdrant];
}

impl fmt::Display for Backend {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Local => "Local index",
            Self::Qdrant => "Qdrant",
        })
    }
}

#[derive(Clone)]
pub(crate) enum Edit {
    Backend(Backend),
    Endpoint(String),
    Collection(String),
    Dimensions(String),
    CredentialVariable(String),
    VaultKey(Option<String>),
}

#[derive(Default)]
pub(super) struct Editor {
    dimensions: Option<String>,
}

impl Editor {
    pub(super) fn apply(&mut self, store: &mut Store, edit: Edit) -> bool {
        if !editable(store) && !matches!(&edit, Edit::CredentialVariable(_) | Edit::VaultKey(_)) {
            return false;
        }
        let previous = store.qdrant.clone();
        let previous_local = store.local_dimensions;
        match (edit, &mut store.qdrant) {
            (Edit::Backend(Backend::Local), qdrant) => {
                *qdrant = None;
                self.dimensions = None;
            }
            (Edit::Backend(Backend::Qdrant), qdrant) => {
                if qdrant.is_none() {
                    self.dimensions = None;
                    store.local_dimensions = store
                        .local_dimensions
                        .filter(|value| (1..=MAX_DIMENSIONS).contains(value));
                }
                qdrant.get_or_insert_with(Settings::default);
            }
            (Edit::Endpoint(value), Some(settings)) => settings.endpoint = value,
            (Edit::Collection(value), Some(settings)) => settings.collection = value,
            (Edit::Dimensions(value), Some(settings)) => {
                settings.dimensions = value.trim().parse().unwrap_or_default();
                self.dimensions = Some(value);
            }
            (Edit::Dimensions(value), None) => {
                store.local_dimensions =
                    (!value.trim().is_empty()).then(|| value.trim().parse().unwrap_or_default());
                self.dimensions = Some(value);
            }
            (Edit::CredentialVariable(value), Some(settings)) if settings.vault_key.is_none() => {
                settings.credential_env = value;
            }
            (Edit::VaultKey(id), Some(settings)) => {
                if id.is_some() {
                    settings.credential_env.clear();
                }
                settings.vault_key = id;
            }
            (Edit::CredentialVariable(_), Some(_)) | (_, None) => return false,
        }
        previous != store.qdrant || previous_local != store.local_dimensions
    }

    pub(super) fn view<'a>(
        &'a self,
        store_index: usize,
        store: &'a Store,
        idle: bool,
        vault_keys: Vec<CredentialChoice>,
        reveal: &Reveal,
    ) -> Element<'a, Message> {
        let editable = editable(store);
        let can_edit = editable && idle;
        let backend = if store.qdrant.is_some() {
            Backend::Qdrant
        } else {
            Backend::Local
        };
        let message = move |edit| Message::KnowledgeBackend {
            store: store_index,
            edit,
        };
        let picker: Element<'_, Message> = if can_edit {
            pick_list(Backend::ALL, Some(backend), move |backend| {
                message(Edit::Backend(backend))
            })
            .dismiss_on_escape()
        } else {
            text(backend.to_string()).into()
        };
        let settings: Element<'_, Message> = if let Some(settings) = &store.qdrant {
            let vault_key = settings.vault_key.as_deref().unwrap_or_default();
            let mut choices = vec![CredentialChoice {
                id: String::new(),
                label: "Environment variable or anonymous".into(),
                username: String::new(),
            }];
            choices.extend(vault_keys);
            if !choices.iter().any(|choice| choice.id == vault_key) {
                choices.push(CredentialChoice {
                    id: vault_key.into(),
                    label: "Saved Qdrant API key".into(),
                    username: String::new(),
                });
            }
            let selected = choices
                .iter()
                .find(|choice| choice.id == vault_key)
                .cloned();
            let key_picker: Element<'_, Message> = if idle {
                pick_list(choices, selected, move |choice: CredentialChoice| {
                    message(Edit::VaultKey((!choice.id.is_empty()).then_some(choice.id)))
                })
                .dismiss_on_escape()
            } else {
                text(selected.map_or_else(String::new, |choice| choice.label)).into()
            };
            column![
                field(
                    "Qdrant endpoint",
                    reveal.knowledge_input(
                        KnowledgeSetting::QdrantEndpoint,
                        can_edit,
                        text_input("HTTPS URL or loopback HTTP URL", &settings.endpoint)
                            .style(dialog_design::form_input)
                            .on_input_maybe(
                                can_edit.then_some(move |value| message(Edit::Endpoint(value)))
                            )
                    ),
                ),
                field(
                    "Qdrant collection",
                    reveal.knowledge_input(
                        KnowledgeSetting::QdrantCollection,
                        can_edit,
                        text_input("Collection name", &settings.collection)
                            .style(dialog_design::form_input)
                            .on_input_maybe(
                                can_edit.then_some(move |value| message(Edit::Collection(value)))
                            )
                    ),
                ),
                reveal.wrap(
                    Setting::Knowledge(KnowledgeSetting::QdrantAuthentication),
                    field("Qdrant API key", key_picker)
                ),
                settings.vault_key.is_none().then(|| field(
                    "API key variable",
                    reveal.knowledge_input(
                        KnowledgeSetting::QdrantCredentialVariable,
                        idle,
                        text_input(
                            "Optional environment variable name",
                            &settings.credential_env
                        )
                        .style(dialog_design::form_input)
                        .on_input_maybe(
                            idle.then_some(move |value| message(Edit::CredentialVariable(value)))
                        )
                    ),
                )),
                settings.vault_key.is_none().then(|| text(
                    "The application reads the API key from the named environment variable."
                )
                .size(13)),
            ]
            .spacing(6)
            .into()
        } else {
            let defaults = Settings::default();
            let inactive_input = |value: &str| {
                text_input::<Message, iced::Theme, iced::Renderer>("", value)
                    .width(SPINNER_WIDTH)
                    .size(13)
                    .style(dialog_design::form_input)
            };
            column![
                field("Qdrant endpoint", inactive_input(&defaults.endpoint)),
                field("Qdrant collection", inactive_input(&defaults.collection)),
                field("Qdrant API key", inactive_input("")),
            ]
            .spacing(6)
            .into()
        };
        let dimensions = self.dimensions.clone().unwrap_or_else(|| {
            store
                .qdrant
                .as_ref()
                .map(|settings| settings.dimensions)
                .or(store.local_dimensions)
                .filter(|value| *value != 0)
                .map_or_else(String::new, |value| value.to_string())
        });
        let (dimension_setting, dimension_placeholder, maximum, initial) = match backend {
            Backend::Local => (
                KnowledgeSetting::LocalDimensions,
                "Automatic",
                MAX_DIMENSIONS,
                1024,
            ),
            Backend::Qdrant => (
                KnowledgeSetting::QdrantDimensions,
                "Dimensions",
                qdrant::MAX_DIMENSIONS,
                Settings::default().dimensions,
            ),
        };
        let numeric = dimensions.trim().parse::<usize>().ok();
        let step = |value: usize| message(Edit::Dimensions(value.to_string()));
        let up = (can_edit && numeric.is_none_or(|value| value < maximum))
            .then(|| step(numeric.map_or(initial, |value| value + 1)));
        let down = (can_edit && numeric.is_none_or(|value| value > 1))
            .then(|| step(numeric.map_or(initial, |value| (value - 1).min(maximum))));
        let dimension_setting = Setting::Knowledge(dimension_setting);
        let spinner = row![
            reveal.input(
                dimension_setting,
                can_edit,
                text_input(dimension_placeholder, &dimensions)
                    .width(Length::Fill)
                    .size(13)
                    .style(dialog_design::form_input)
                    .on_input_maybe(
                        can_edit.then_some(move |value| message(Edit::Dimensions(value)))
                    ),
            ),
            column![number_step("▴", up), number_step("▾", down)],
        ]
        .align_y(Alignment::Center)
        .width(SPINNER_WIDTH);
        let form = column![
            reveal.wrap(Setting::Knowledge(KnowledgeSetting::VectorStorage), field("Vector storage", picker)),
            reveal.wrap(dimension_setting, field("Embedding dimensions", spinner)),
            settings,
            (!editable).then(|| text(
                "Remove the store's sources before changing the backend, endpoint, collection, or embedding dimensions."
            )
            .size(13)),
        ]
        .spacing(6);
        container(form)
            .padding(8)
            .width(Length::Fill)
            .style(dialog_design::page)
            .into()
    }
}

fn field<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![text(label).size(13).width(145), control.into()]
        .align_y(Alignment::Center)
        .spacing(8)
        .into()
}

pub(super) fn editable(store: &Store) -> bool {
    store.sources.is_empty() && store.folders.is_empty()
}

pub(super) fn validate(store: &Store) -> Result<(), String> {
    store.validate_local_dimensions()?;
    match &store.qdrant {
        Some(settings) => settings.validate(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn knowledge_setting_search_uses_the_selected_store_and_focuses_its_qdrant_endpoint() {
        use std::{path::PathBuf, sync::Arc};

        use iced::{Font, Renderer, Size, keyboard::Key};
        use iced_runtime::{UserInterface, user_interface::Cache};

        use crate::ai_manager::{Configuration, Dialog, Message as ManagerMessage, search::Target};
        use crate::settings_search::test_input;

        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let send = |dialog: &mut Dialog, message| dialog.update(ManagerMessage::Local(message));
        for name in ["Local fixture", "Qdrant fixture"] {
            let _ = send(&mut dialog, Message::StoreName(name.into()));
            let _ = send(&mut dialog, Message::CreateStore);
        }
        let _ = send(&mut dialog, Message::SelectStore(1));
        let _ = send(
            &mut dialog,
            Message::KnowledgeBackend {
                store: 1,
                edit: Edit::Backend(Backend::Qdrant),
            },
        );
        let endpoint = Setting::Knowledge(KnowledgeSetting::QdrantEndpoint);
        let _ = send(&mut dialog, Message::SelectStore(0));
        let _ = dialog.update(ManagerMessage::Search("qdrant endpoint".into()));
        assert!(
            !dialog
                .search_results()
                .any(|result| result == Target::Setting(endpoint))
        );
        let _ = dialog.update(ManagerMessage::SearchEscape);

        let _ = send(&mut dialog, Message::SelectStore(1));
        let _ = dialog.update(ManagerMessage::Search("qdrant endpoint".into()));
        assert_eq!(
            dialog.search_results().next(),
            Some(Target::Setting(endpoint))
        );
        let before = serde_json::to_value(&dialog.local_state).unwrap();
        let reveal = dialog.update(ManagerMessage::SearchChoose);
        assert_eq!(dialog.revealed.setting, Some(endpoint));
        assert_eq!(serde_json::to_value(&dialog.local_state).unwrap(), before);
        assert!(dialog.local.request.is_none());
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            Size::new(760.0, 360.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(
            matches!(messages.as_slice(), [ManagerMessage::Local(Message::KnowledgeBackend { store: 1, edit: Edit::Endpoint(value) })] if value.ends_with('x'))
        );
        drop(interface);

        let _ = dialog.update(ManagerMessage::Search("folder include globs".into()));
        let _ = dialog.update(ManagerMessage::SearchChoose);
        assert_eq!(
            dialog.revealed.setting,
            Some(Setting::Knowledge(KnowledgeSetting::FolderIncludes))
        );
        assert!(dialog.local.request.is_none());
        assert_eq!(serde_json::to_value(&dialog.local_state).unwrap(), before);
    }
    use super::super::{Dialog, SourceKind, State, SyncMode};
    use super::*;
    use std::io::{self, Write as _};

    use iced::futures::{StreamExt as _, executor::block_on};
    use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};

    use crate::local_ai::test_support;
    use crate::vault_window::{self, VaultWindow};

    fn approve_file(mut update: impl FnMut(Message) -> iced::Task<Message>) {
        for action in [Message::AddSource, Message::ApproveBrowsedKnowledgeFiles] {
            let mut events = iced_runtime::task::into_stream(update(action)).unwrap();
            let Some(iced_runtime::Action::Output(result @ Message::Finished(_, Ok(_)))) =
                block_on(events.next())
            else {
                panic!("file review did not finish successfully");
            };
            let _ = update(result);
        }
    }

    #[test]
    fn qdrant_draft_save_requires_the_reviewed_vault_revision() {
        let _exclusive = test_support::exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let vault_path = profile.0.join("vault.bin");
        let master = b"fixture vault passphrase";
        let mut vault = Vault::create(&vault_path, master).unwrap();
        let id = vault
            .save(
                CredentialInput {
                    id: None,
                    label: "Fixture Qdrant key".into(),
                    username: String::new(),
                    server_pattern: String::new(),
                    environment: String::new(),
                    description: String::new(),
                    kind: CredentialKind::QdrantApiKey,
                },
                Secret::new("fixture-qdrant-api-key".into()),
            )
            .unwrap();
        let mut vault = VaultWindow::with_unlocked_fixture(vault_path.clone(), vault);

        let (listener, _) = test_support::listener();
        let source = profile.0.join("notes.txt");
        std::fs::write(&source, "Index fixture").unwrap();
        let saved = State::default();
        let mut state = State {
            endpoint: listener.local_addr().unwrap().to_string(),
            embedding: "fixture-embedding".into(),
            stores: vec![Store::new("Draft Qdrant store").unwrap()],
            ..State::default()
        };
        let mut dialog = Dialog::new(profile.0.clone());
        let edit = |edit| Message::KnowledgeBackend { store: 0, edit };
        for message in [
            Message::SelectStore(0),
            edit(Edit::Backend(Backend::Qdrant)),
            edit(Edit::CredentialVariable("KORTTY_FIXTURE_QDRANT".into())),
            edit(Edit::Dimensions("3".into())),
            Message::Source(source.display().to_string()),
        ] {
            let _ = dialog.update_with_vault(message, &mut state, &saved, true, Some(&vault));
        }
        approve_file(|message| {
            dialog.update_with_vault(message, &mut state, &saved, true, Some(&vault))
        });
        for message in [
            edit(Edit::VaultKey(Some(id.clone()))),
            edit(Edit::CredentialVariable("KORTTY_STALE_QDRANT".into())),
            Message::Consent(true),
        ] {
            let _ = dialog.update_with_vault(message, &mut state, &saved, true, Some(&vault));
        }
        let settings = state.stores[0].qdrant.as_ref().unwrap();
        assert_eq!(settings.vault_key.as_deref(), Some(id.as_str()));
        assert!(settings.credential_env.is_empty());
        assert!(dialog.consent_current());

        let server = std::thread::spawn(move || {
            let mut stream = test_support::accept(&listener);
            test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
            let response =
                test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0,0]]}"#);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        });
        let index = dialog.update_with_vault(
            Message::Index { rebuild: false },
            &mut state,
            &saved,
            true,
            Some(&vault),
        );
        let mut events = iced_runtime::task::into_stream(index).unwrap();
        let Some(iced_runtime::Action::Output(message)) = block_on(events.next()) else {
            panic!("draft indexing did not finish");
        };
        assert!(matches!(message, Message::Finished(_, Ok(_))));
        let _ = dialog.update_with_vault(message, &mut state, &saved, true, Some(&vault));
        server.join().unwrap();
        assert_eq!(dialog.drafts.len(), 1);
        let prepared = dialog
            .prepare_save_indexes(&state, &saved, Some(&vault), true)
            .unwrap();

        let _ = vault.update(vault_window::Message::Lock);
        assert!(!dialog.consent_current());
        assert!(
            dialog
                .prepare_save_indexes(&state, &saved, Some(&vault), true)
                .is_err()
        );
        assert!(
            dialog
                .prepare_save_indexes(&state, &saved, Some(&vault), false)
                .unwrap()
                .staged
                .is_empty()
        );
        assert!(
            prepared
                .commit_checked(
                    |_| panic!("revoked settings were published"),
                    || Ok(()),
                    None,
                    true
                )
                .is_err()
        );
        let scope = state.scope(&state.stores[0], &profile.0).unwrap();
        assert_eq!(
            dialog.cache.load(&scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        let reopened = Vault::unlock(&vault_path, master).unwrap();
        let reopened = VaultWindow::with_unlocked_fixture(vault_path, reopened);
        assert!(
            dialog
                .prepare_save_indexes(&state, &saved, Some(&reopened), true)
                .is_err()
        );
    }

    #[test]
    fn knowledge_backend_edits_gate_sources_and_bind_automatic_destinations() {
        let profile = crate::snippets::tests::Profile::new();
        let source = profile.0.join("runbook.txt");
        std::fs::write(&source, "Operations runbook").unwrap();
        let saved = State {
            embedding: "fixture-embedding".into(),
            stores: vec![Store::new("Runbooks").unwrap()],
            ..State::default()
        };
        let mut state = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let dispatch = |dialog: &mut Dialog, state: &mut State, message| {
            dialog.update(message, state, &saved, true)
        };
        let edit = |edit| Message::KnowledgeBackend { store: 0, edit };
        let _ = dispatch(&mut dialog, &mut state, Message::SelectStore(0));
        let _ = dispatch(&mut dialog, &mut state, Message::Consent(true));
        assert!(dialog.consent);
        let _ = dispatch(
            &mut dialog,
            &mut state,
            edit(Edit::Backend(Backend::Qdrant)),
        );
        assert!(!dialog.consent);

        let _ = dispatch(
            &mut dialog,
            &mut state,
            edit(Edit::Dimensions("invalid".into())),
        );
        assert!(state.validate().is_err());
        let _ = dispatch(
            &mut dialog,
            &mut state,
            Message::Source(source.display().to_string()),
        );
        let _ = dispatch(&mut dialog, &mut state, Message::AddSource);
        assert!(state.stores[0].sources.is_empty());
        let review = dispatch(&mut dialog, &mut state, Message::ReviewFolder);
        assert!(iced_runtime::task::into_stream(review).is_none());
        assert!(dialog.status.is_some());

        let _ = dispatch(&mut dialog, &mut state, edit(Edit::Dimensions("3".into())));
        approve_file(|message| dispatch(&mut dialog, &mut state, message));
        assert_eq!(
            state.stores[0].sources.as_slice(),
            std::slice::from_ref(&source)
        );
        let reviewed = state.stores[0].qdrant.clone().unwrap();
        assert_eq!(reviewed.dimensions, 3);
        let _ = dispatch(
            &mut dialog,
            &mut state,
            edit(Edit::Collection("another_collection".into())),
        );
        let _ = dispatch(&mut dialog, &mut state, edit(Edit::Backend(Backend::Local)));
        assert!(state.stores[0].qdrant.as_ref() == Some(&reviewed));
        let _ = dispatch(
            &mut dialog,
            &mut state,
            Message::SelectKnowledgeSource(0, SourceKind::File, source),
        );
        let _ = dispatch(&mut dialog, &mut state, Message::OpenSourceSync(0));
        let _ = dispatch(
            &mut dialog,
            &mut state,
            Message::SourceSyncMode(SyncMode::Automatic),
        );
        let _ = dispatch(&mut dialog, &mut state, Message::ApplySourceSync);
        assert!(!state.automatic_ready(&state.stores[0]));
        let _ = dispatch(
            &mut dialog,
            &mut state,
            Message::ApproveAutomaticEmbedding(0),
        );
        assert!(state.stores[0].automatic_qdrant.as_ref() == Some(&reviewed));
        assert!(state.automatic_ready(&state.stores[0]));
        let _ = dispatch(&mut dialog, &mut state, Message::Consent(true));
        assert!(dialog.consent);
        let _ = dispatch(
            &mut dialog,
            &mut state,
            edit(Edit::CredentialVariable("KORTTY_REPLACEMENT_QDRANT".into())),
        );
        assert_eq!(state.stores[0].sources.len(), 1);
        assert_eq!(
            state.stores[0].qdrant.as_ref().unwrap().credential_env,
            "KORTTY_REPLACEMENT_QDRANT"
        );
        assert!(!dialog.consent);
        assert!(state.stores[0].automatic_embedding.is_none());
        assert!(state.stores[0].automatic_qdrant.is_none());
        assert!(!state.automatic_ready(&state.stores[0]));
        assert!(state.validate().is_ok());
    }
}
