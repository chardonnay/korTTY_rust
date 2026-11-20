//! Binds an imported Java profile to a reviewed native model registration.

use super::*;

pub(super) struct Review {
    ticket: Arc<()>,
    profile: ProfileId,
    model: String,
    _worker: ModelInventory,
}

impl Profile {
    pub(super) fn clear_unreviewed_embedded_model(&mut self) {
        if self.restore_review && self.java_embedded_model.is_some() {
            self.model.clear();
        }
    }

    fn managed_runtime(&self) -> PolicyModelRuntime {
        if self.is_mlx() {
            PolicyModelRuntime::Mlx
        } else {
            PolicyModelRuntime::Gguf
        }
    }
}

impl Catalog {
    pub(in crate::ai_manager) fn require_imported_model_changes(
        &self,
        root: &Path,
        baseline: &Self,
        policy: &ClipboardPolicy,
    ) -> io::Result<()> {
        for profile in &self.profiles {
            let previous = baseline.user_profile(profile.id);
            if profile.is_mlx()
                && !profile.restore_review
                && let Some(effort) = profile.reasoning.api_value()
                && !previous.is_some_and(|old| {
                    !old.restore_review
                        && old.is_mlx()
                        && old.model == profile.model
                        && old.reasoning == profile.reasoning
                })
            {
                let controls = check_mlx_reasoning(root, &profile.model, policy)?;
                if !controls.supports(Some(effort)) {
                    return Err(io::Error::other(mlx_models::UNSUPPORTED_REASONING));
                }
            }
            if profile.restore_review
                || profile.java_embedded_model.is_none()
                || previous.is_some_and(|old| {
                    !old.restore_review
                        && old.connection_mode == profile.connection_mode
                        && old.model == profile.model
                        && old.vision == profile.vision
                        && old.java_embedded_model == profile.java_embedded_model
                        && old.java_source == profile.java_source
                })
            {
                continue;
            }
            let vision =
                check_registration(root, profile.managed_runtime(), &profile.model, policy)
                    .map_err(io::Error::other)?;
            if profile.vision == Vision::Enabled && !vision {
                return Err(io::Error::other(
                    "The selected model has no vision projector.",
                ));
            }
        }
        Ok(())
    }
}

fn check_mlx_reasoning(
    root: &Path,
    model: &str,
    policy: &ClipboardPolicy,
) -> io::Result<mlx_models::ReasoningControls> {
    policy.require_ai_profile_edit()?;
    if !mlx_runtime::supported() {
        return Err(io::Error::other(
            "Managed MLX requires an Apple Silicon Mac.",
        ));
    }
    if valid_policy_model_id(model) {
        let authority =
            local_ai::PolicyModelAuthority::resolve(root, model, PolicyModelRuntime::Mlx)?;
        policy_models::mlx_reasoning(authority.definition(), &AtomicBool::new(false))
    } else {
        policy.require_ai_user_model_files()?;
        mlx_models::Registry::new(root)
            .open(model)
            .map(|opened| opened.reasoning_controls())
    }
}

impl Editor {
    pub(in crate::ai_manager) fn review_imported_model(
        &mut self,
        root: PathBuf,
        id: ProfileId,
        model: &str,
    ) -> Task<Message> {
        let Some(profile) = self.selected().filter(|profile| {
            profile.id == id
                && profile.model == model
                && profile.is_managed()
                && profile.restore_review
                && profile.java_embedded_model.is_some()
                && profile.java_source.is_some()
                && (!profile.is_mlx() || mlx_runtime::supported())
        }) else {
            return Task::none();
        };
        let inventory = self.managed_catalog(profile);
        if self.wizard_open()
            || self.imported_model_review.is_some()
            || inventory.loading
            || !inventory.choices.iter().any(|choice| choice.id == model)
        {
            return Task::none();
        }
        let runtime = profile.managed_runtime();
        let model = model.to_owned();
        let selected = model.clone();
        let ticket = Arc::new(());
        let event_ticket = Arc::clone(&ticket);
        let (worker, task) = ModelInventory::start(
            move |cancelled| {
                if cancelled.load(Ordering::Acquire) {
                    return Err("The imported model review was cancelled.".into());
                }
                check_registration(&root, runtime, &selected, &ClipboardPolicy::load())
            },
            || Err("Could not check the registered model.".into()),
            move |result| Message::ImportedModelChecked(event_ticket, result),
        );
        self.imported_model_review = Some(Review {
            ticket,
            profile: id,
            model,
            _worker: worker,
        });
        self.set_status("Checking the registered model…");
        task
    }

    pub(super) fn finish_imported_model_review(
        &mut self,
        ticket: &Arc<()>,
        result: Result<bool, String>,
        editable: bool,
    ) {
        if self
            .imported_model_review
            .as_ref()
            .is_none_or(|review| !Arc::ptr_eq(&review.ticket, ticket))
        {
            return;
        }
        let review = self.imported_model_review.take().unwrap();
        if !editable {
            self.set_status("Review the imported model when AI profiles can be edited.");
            return;
        }
        let Some(profile) = self.selected_mut().filter(|profile| {
            profile.id == review.profile && profile.model == review.model && profile.restore_review
        }) else {
            return;
        };
        match result {
            Ok(vision) => {
                if !vision {
                    profile.vision = Vision::Disabled;
                }
                profile.restore_review = false;
                self.set_status("Model reviewed. Save to use it with this profile.");
            }
            Err(error) => self.set_status(error),
        }
    }
}

fn check_registration(
    root: &Path,
    runtime: PolicyModelRuntime,
    model: &str,
    policy: &ClipboardPolicy,
) -> Result<bool, String> {
    policy
        .require_ai_profile_edit()
        .map_err(|error| error.to_string())?;
    if runtime == PolicyModelRuntime::Mlx && !mlx_runtime::supported() {
        return Err("Managed MLX requires an Apple Silicon Mac.".into());
    }
    if valid_policy_model_id(model) {
        local_ai::PolicyModelAuthority::resolve(root, model, runtime)
            .map_err(|error| error.to_string())?;
        return Ok(false);
    }
    policy
        .require_ai_user_model_files()
        .map_err(|error| error.to_string())?;
    let result = match runtime {
        PolicyModelRuntime::Gguf => managed_models::Registry::new(root.to_owned())
            .open(model)
            .and_then(|opened| {
                if opened.model.purpose != managed_models::Purpose::Chat {
                    return Err(io::Error::other("Choose a GGUF chat model"));
                }
                Ok(opened.model.projector_bytes.is_some())
            }),
        PolicyModelRuntime::Mlx => mlx_models::Registry::new(root).open(model).map(|_| false),
    };
    result.map_err(|error| format!("Could not review the selected model ({error})."))
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt as _, executor::block_on};

    use super::*;
    use crate::ai_manager::{Configuration, backup};
    use crate::snippets::tests::Profile as Scratch;

    const JAVA: &[u8] = br#"<globalSettings>
        <sessionJournalAiProfileId>java-local</sessionJournalAiProfileId>
        <aiProfiles><profile><id>java-local</id><name>Imported work</name>
            <connectionMode>EMBEDDED_LLAMA_CPP</connectionMode>
            <embeddedModelId>java-model</embeddedModelId>
        </profile></aiProfiles>
    </globalSettings>"#;

    fn complete(task: Task<Message>) -> Message {
        let mut actions = iced_runtime::task::into_stream(task).unwrap();
        let iced_runtime::Action::Output(message) = block_on(actions.next()).unwrap() else {
            panic!("expected model review result");
        };
        message
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    #[ignore = "downloads a signed MLX release and requires KORTTY_SMOKE_MLX_MODEL"]
    fn native_java_qwen_reasoning_adoption() {
        native_java_mlx_reasoning("KORTTY_SMOKE_MLX_MODEL", "NONE", Reasoning::None, false);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    #[ignore = "downloads a signed MLX release and requires KORTTY_SMOKE_MLX_GPT_OSS_MODEL"]
    fn native_java_gpt_oss_reasoning_adoption() {
        native_java_mlx_reasoning(
            "KORTTY_SMOKE_MLX_GPT_OSS_MODEL",
            "HIGH",
            Reasoning::High,
            true,
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn native_java_mlx_reasoning(
        source_variable: &str,
        java_effort: &str,
        effort: Reasoning,
        expects_reasoning: bool,
    ) {
        use crate::local_ai::runtime_package::with_stopped_mlx_model;

        let source = PathBuf::from(std::env::var_os(source_variable).expect(source_variable));
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let cancelled = AtomicBool::new(false);
        let java = format!(
            "<globalSettings><aiProfiles><profile>
               <id>java-mlx</id><name>Imported MLX</name>
               <connectionMode>EMBEDDED_MLX</connectionMode>
               <embeddedModelId>previous-mlx</embeddedModelId>
               <reasoningEffort>{java_effort}</reasoningEffort>
             </profile></aiProfiles></globalSettings>"
        );
        let (portable, _) = backup::portable_java_profiles(java.as_bytes()).unwrap();
        let merged = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        let draft = Configuration::decode(&merged).unwrap();
        draft
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let draft = Configuration::load_fixture_from(&root).unwrap();
        let reference = ProfileRef::Numeric(1);
        let source_profile = draft.java_profile_source(&reference).unwrap();
        assert!(draft.terminal_chat_transport(1).is_err());

        let registry = mlx_models::Registry::new(&root);
        let model = registry
            .import(
                &mlx_models::Review::prepare(&source, &cancelled).unwrap(),
                &cancelled,
            )
            .unwrap();
        let mut editor = Editor::new(draft.profiles.clone());
        let runtime = PolicyModelRuntime::Mlx;
        editor.update(
            Message::ManagedModelsLoaded(runtime, ManagedModels::load(&root, runtime, &cancelled)),
            true,
        );
        editor.update(Message::ManagedModel(model.id.clone()), true);
        let imported_effort = editor
            .selected()
            .unwrap()
            .java_embedded_model
            .as_ref()
            .unwrap()
            .mlx_reasoning();
        assert_eq!(imported_effort, Some(effort));
        editor.update(Message::Reasoning(effort), true);
        assert_eq!(editor.selected().unwrap().reasoning, Reasoning::Default);
        let review = editor.review_imported_model(root.clone(), 1, &model.id);
        editor.update(complete(review), true);
        let reviewed = editor.selected().unwrap();
        assert!(!reviewed.restore_review);
        assert_eq!(reviewed.reasoning, Reasoning::Default);
        editor.update(Message::Reasoning(effort), true);

        let settings = Configuration {
            profiles: editor.validated_catalog().unwrap(),
            ..draft.clone()
        };
        settings.save_fixture_at(&root, &draft).unwrap();
        let settings = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(settings.profiles.profile(1).unwrap().reasoning, effort);
        assert_eq!(
            settings.java_profile_source(&reference),
            Some(source_profile)
        );

        let pending = mlx_runtime::online::check(root.clone()).unwrap();
        let catalog = pending.reply.recv_blocking().unwrap().unwrap();
        let choice = catalog
            .choices()
            .first()
            .expect("A compatible signed runtime")
            .clone();
        println!("Runtime {choice}");
        let pending = mlx_runtime::online::download(root.clone(), catalog, choice).unwrap();
        assert!(pending.reply.recv_blocking().unwrap().unwrap().activated);
        let (profile, _) = settings.terminal_chat_transport(1).unwrap();
        assert_eq!(profile.usage_estimate_profile().0, model.id);
        let client = local_ai::Client::new(profile);
        let mut reasoning = String::new();
        let answer = client.stream_chat(
            "Answer in one word.",
            &[],
            "What is the capital of France?",
            || Ok(()),
            |delta| {
                if let local_ai::StreamDelta::Reasoning(part) = delta {
                    reasoning.push_str(&part);
                }
            },
        );
        drop(client);
        with_stopped_mlx_model(&root, &model.id, || Ok(())).unwrap();
        let answer = answer.unwrap();
        assert!(
            answer
                .trim()
                .trim_end_matches('.')
                .eq_ignore_ascii_case("Paris"),
            "{answer:?}"
        );
        assert_eq!(!reasoning.trim().is_empty(), expects_reasoning);
        println!(
            "Java {java_effort} adopted after model review, persisted and used in native inference (reasoning bytes {})",
            reasoning.len()
        );
    }

    #[test]
    fn imported_java_embedded_mlx_review_uses_the_native_platform_gate() {
        let mlx = String::from_utf8_lossy(JAVA).replace("EMBEDDED_LLAMA_CPP", "EMBEDDED_MLX");
        let (catalog, _) = java_backup::decode(mlx.as_bytes()).unwrap();
        let mut editor = Editor::new(catalog);
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let stop = AtomicBool::new(false);
        let input = mlx_models::test_fixture(&root);
        let review = mlx_models::Review::prepare(&input, &stop).unwrap();
        let registry = mlx_models::Registry::new(&root);
        let model = registry.import(&review, &stop).unwrap().id;
        editor.update(
            Message::ManagedModelsLoaded(
                PolicyModelRuntime::Mlx,
                ManagedModels::load(&root, PolicyModelRuntime::Mlx, &stop),
            ),
            true,
        );
        editor.update(Message::ManagedModel(model.clone()), true);
        let task = editor.review_imported_model(root, 1, &model);
        if mlx_runtime::supported() {
            editor.update(complete(task), true);
            let reviewed = editor.validated_catalog().unwrap();
            let profile = reviewed.profile(1).unwrap();
            assert!(!profile.restore_review);
            assert_eq!(profile.model, model);
            assert!(profile.vision == Vision::Disabled);
        } else {
            assert!(iced_runtime::task::into_stream(task).is_none());
            let profile = editor.selected().unwrap();
            assert!(profile.restore_review);
            assert!(profile.model.is_empty());
        }
    }

    #[test]
    fn imported_java_embedded_review_binds_live_chat_registration_and_preserves_source() {
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let model_path = root.join("same name.gguf");
        std::fs::write(&model_path, managed_models::test_fixture(2048, 1)).unwrap();
        let registry = managed_models::Registry::new(root.clone());
        let stop = AtomicBool::new(false);
        let register = |purpose| registry.import(&model_path, purpose, &stop).unwrap().id;
        let chat = register(managed_models::Purpose::Chat);
        let replacement = register(managed_models::Purpose::Chat);
        let embedding = register(managed_models::Purpose::Embedding);
        let check = |id| {
            check_registration(
                &root,
                PolicyModelRuntime::Gguf,
                id,
                &ClipboardPolicy::default(),
            )
        };
        assert!(check(&embedding).unwrap_err().contains("GGUF chat model"));

        let (portable, _) = backup::portable_java_profiles(JAVA).unwrap();
        let merged = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        let draft = Configuration::decode(&merged).unwrap();
        draft
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let draft = Configuration::load_fixture_from(&root).unwrap();
        let source = draft
            .java_profile_source(&ProfileRef::Numeric(1))
            .unwrap()
            .clone();
        assert!(draft.agent_transport(1).is_err());
        assert!(draft.journal_summary_transport(1, false).is_err());

        let mut editor = Editor::new(draft.profiles.clone());
        editor.update(
            Message::ManagedModelsLoaded(
                PolicyModelRuntime::Gguf,
                ManagedModels::load(&root, PolicyModelRuntime::Gguf, &stop),
            ),
            true,
        );
        editor.update(Message::ManagedModel(embedding), true);
        assert!(editor.selected().unwrap().model.is_empty());
        use crate::ai_manager::search::ProfileAction;
        assert!(!editor.can_reveal_profile_action(ProfileAction::ConfirmImportedModel, true));
        editor.update(Message::ManagedModel(chat.clone()), true);
        assert!(editor.can_reveal_profile_action(ProfileAction::ConfirmImportedModel, true));
        assert!(!editor.can_reveal_profile_action(ProfileAction::ConfirmImportedModel, false));
        let candidate = editor.validated_catalog().unwrap();
        assert!(candidate.profile(1).unwrap().model.is_empty());
        assert!(candidate.choices().is_empty());

        let review = editor.review_imported_model(root.clone(), 1, &chat);
        editor.update(Message::ManagedModel(replacement.clone()), true);
        editor.update(complete(review), true);
        assert!(editor.selected().unwrap().restore_review);
        editor.update(Message::ManagedModel(chat.clone()), true);
        let review = editor.review_imported_model(root.clone(), 1, &chat);
        editor.update(complete(review), true);
        let catalog = editor.validated_catalog().unwrap();
        assert_eq!(catalog.choices(), [(1, "Imported work".to_owned())]);
        let mapped = Configuration {
            profiles: catalog,
            ..draft.clone()
        };
        mapped.save_fixture_at(&root, &draft).unwrap();
        let mapped = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(
            mapped
                .agent_transport(1)
                .unwrap()
                .0
                .usage_estimate_profile()
                .0,
            chat
        );
        assert_eq!(
            mapped
                .journal_summary_transport(1, false)
                .unwrap()
                .usage_estimate_profile()
                .0,
            chat
        );
        assert_eq!(
            mapped
                .terminal_chat_transport(1)
                .unwrap()
                .0
                .usage_estimate_profile()
                .0,
            chat
        );
        assert!(mapped.default_ai_profile_reference().is_none());
        assert_eq!(
            mapped.resolve_imported_journal_summary_profile(false),
            Some(1)
        );
        assert_eq!(
            mapped.java_profile_source(&ProfileRef::Numeric(1)),
            Some(&source)
        );

        let native_backup =
            backup::portable_settings(&serde_json::to_vec(&mapped).unwrap()).unwrap();
        let restored = Configuration::decode(&native_backup).unwrap();
        assert_eq!(
            restored.java_profile_source(&ProfileRef::Numeric(1)),
            Some(&source)
        );
        assert!(restored.profiles.profile(1).unwrap().model.is_empty());
        assert!(restored.agent_transport(1).is_err());
        assert!(
            backup::merge_java_profiles(
                Some(&serde_json::to_vec(&mapped).unwrap()),
                &portable,
                &ClipboardPolicy::default()
            )
            .unwrap()
            .is_none()
        );

        editor.update(Message::ManagedModel(replacement.clone()), true);
        let review = editor.review_imported_model(root.clone(), 1, &replacement);
        editor.update(complete(review), true);
        let rebound = Configuration {
            profiles: editor.validated_catalog().unwrap(),
            ..mapped.clone()
        };
        registry.remove(&replacement).unwrap();
        assert!(rebound.save_fixture_at(&root, &mapped).is_err());
        let saved = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(
            saved
                .agent_transport(1)
                .unwrap()
                .0
                .usage_estimate_profile()
                .0,
            chat
        );
        editor.update(Message::ManagedModel(chat.clone()), true);
        let review = editor.review_imported_model(root.clone(), 1, &chat);
        editor.update(complete(review), false);
        assert!(editor.selected().unwrap().restore_review);
    }
}
