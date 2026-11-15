//! Read only metadata for the exact LM Studio selection in the profile editor.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use iced::Task;

use super::{ClipboardPolicy, Dialog, Message, Page, local_ai, profiles};

const QUIET_TIME: Duration = Duration::from_millis(400);

#[derive(Clone, Eq, PartialEq)]
struct Selection {
    key: profiles::LmStudioDiscoveryKey,
    reference: String,
}

pub(super) struct Lookup {
    ticket: u64,
    selection: Selection,
    state: State,
}

enum State {
    Waiting(Instant),
    Running { _cancel: local_ai::Cancel },
    Unavailable,
}

pub(super) fn discover(
    profile: &profiles::Profile,
) -> Result<local_ai::Pending<local_ai::LmStudioCatalog>, String> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !policy.ai_allowed {
        return Err("Managed policy blocks AI model discovery.".into());
    }
    profile.validate_connection()?;
    if let Some(definition) = profile.policy_definition() {
        local_ai::discover_policy_lm_studio_catalog(definition)
    } else {
        local_ai::discover_lm_studio_catalog(&profile.lm_studio_endpoint()?, profile.credential()?)
    }
}

impl Dialog {
    fn lm_studio_selection(&self) -> Option<Selection> {
        if self.page != Page::Profiles
            || !self.search.trim().is_empty()
            || !self.editable()
            || self.profile_form_lock().is_some()
            || !self.general.features_enabled
            || !self.profiles.model_section_open()
            || self.request.is_some()
        {
            return None;
        }
        let profile = self.profiles.selected()?;
        let reference = profile.lm_studio_reference();
        if !profile.ordinary_lm_studio_auto() && !local_ai::valid_model(reference) {
            return None;
        }
        Some(Selection {
            key: profile.lm_studio_discovery_key()?,
            reference: if profile.ordinary_lm_studio_auto() {
                profile.model.trim().into()
            } else {
                reference.to_owned()
            },
        })
    }

    pub(super) fn finish_ordinary_auto_model_test(
        &mut self,
        ticket: u64,
        result: Result<local_ai::LmStudioAutoModel, String>,
    ) -> Task<Message> {
        if self
            .request
            .as_ref()
            .is_none_or(|request| request.ticket != ticket)
        {
            return Task::none();
        }
        let request = self.request.take().expect("matching request ticket");
        if self.profiles.selected() != Some(&request.profile)
            || !request.profile.ordinary_lm_studio_auto()
        {
            self.notice = "Profile changed. Run the test again for the new settings.".into();
            return Task::none();
        }
        if let Some(lock) = self.profile_form_lock() {
            self.notice = lock.into();
            return Task::none();
        }
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !policy.ai_allowed {
            self.notice = "Managed policy blocks AI model discovery.".into();
            return Task::none();
        }
        let problem = result.as_ref().err().cloned();
        self.profiles.set_selected_lm_studio_auto(result);
        if let Some(problem) = problem {
            self.notice = problem;
            Task::none()
        } else {
            self.request_with_resolution(false, false)
        }
    }

    pub(super) fn refresh_lm_studio_metadata(&mut self) -> Task<Message> {
        let Some(selection) = self.lm_studio_selection() else {
            self.lm_studio_metadata = None;
            return Task::none();
        };
        if self.profiles.lm_studio_catalog_is_fresh(&selection.key) {
            self.lm_studio_metadata = None;
            return Task::none();
        }
        if let Some(lookup) = &mut self.lm_studio_metadata {
            if selection == lookup.selection {
                return Task::none();
            }
            if let State::Waiting(quiet_until) = &mut lookup.state {
                *quiet_until = Instant::now() + QUIET_TIME;
                lookup.selection = selection;
                return Task::none();
            }
        }
        let ticket = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        self.lm_studio_metadata = Some(Lookup {
            ticket,
            selection,
            state: State::Waiting(Instant::now() + QUIET_TIME),
        });
        due_after(ticket, QUIET_TIME)
    }

    pub(super) fn start_lm_studio_metadata(&mut self, ticket: u64) -> Task<Message> {
        let current = self.lm_studio_selection();
        let Some(lookup) = self
            .lm_studio_metadata
            .as_mut()
            .filter(|lookup| lookup.ticket == ticket)
        else {
            return Task::none();
        };
        if current.as_ref() != Some(&lookup.selection) {
            self.lm_studio_metadata = None;
            return Task::none();
        }
        let State::Waiting(quiet_until) = lookup.state else {
            return Task::none();
        };
        let wait = quiet_until.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            return due_after(ticket, wait);
        }
        let Some(profile) = self.profiles.selected() else {
            return Task::none();
        };
        match discover(profile) {
            Ok(local_ai::Pending { reply, cancel }) => {
                lookup.state = State::Running { _cancel: cancel };
                Task::perform(
                    async move {
                        reply
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("Model discovery stopped.".into()))
                            .map(Arc::new)
                    },
                    move |result| Message::LmStudioMetadataReady(ticket, result),
                )
            }
            Err(_) => {
                lookup.state = State::Unavailable;
                Task::none()
            }
        }
    }

    pub(super) fn finish_lm_studio_metadata(
        &mut self,
        ticket: u64,
        result: Result<Arc<local_ai::LmStudioCatalog>, String>,
    ) {
        let current = self.lm_studio_selection();
        let Some(lookup) = self.lm_studio_metadata.as_mut().filter(|lookup| {
            lookup.ticket == ticket && matches!(lookup.state, State::Running { .. })
        }) else {
            return;
        };
        if current.as_ref() != Some(&lookup.selection) {
            self.lm_studio_metadata = None;
            return;
        }
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !policy.ai_allowed {
            self.lm_studio_metadata = None;
            return;
        }
        if let Ok(catalog) = result {
            self.profiles
                .set_lm_studio_catalog(lookup.selection.key.clone(), catalog, false);
            self.lm_studio_metadata = None;
        } else {
            lookup.state = State::Unavailable;
        }
    }
}

fn due_after(ticket: u64, duration: Duration) -> Task<Message> {
    let (send, receive) = async_channel::bounded(1);
    if thread::Builder::new()
        .name("kortty-lm-studio-editor-delay".into())
        .spawn(move || {
            thread::sleep(duration);
            let _ = send.try_send(());
        })
        .is_err()
    {
        return Task::none();
    }
    Task::future(async move {
        let _ = receive.recv().await;
        Message::LmStudioMetadataDue(ticket)
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Write};
    use std::path::PathBuf;

    use iced::futures::{StreamExt, executor::block_on};
    use local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, listener, wire_request,
    };

    use super::*;
    use crate::ai_manager::Configuration;
    use crate::ai_manager::profiles::{
        ConnectionMode, Field, Message as ProfileMessage, ModelMode, Reasoning, Section,
    };

    fn output(task: Task<Message>, expected: impl Fn(&Message) -> bool) -> Message {
        let mut actions = iced_runtime::task::into_stream(task).expect("metadata work scheduled");
        while let Some(action) = block_on(actions.next()) {
            if let iced_runtime::Action::Output(message) = action
                && expected(&message)
            {
                return message;
            }
        }
        panic!("metadata work did not report its result");
    }

    #[test]
    fn lm_studio_editor_uses_exact_loaded_alias_and_discards_a_stale_metadata_reply() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap();
        let address = format!("http://localhost:{}/api/v1/chat", endpoint.port());
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "GET /api/v1/models HTTP/1.1");
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                if line.trim_end().is_empty() {
                    break;
                }
            }
            let body = r#"{"models":[
                {"type":"llm","key":"vision","loaded_instances":[{"id":"vision-live"}],"capabilities":{"vision":true,"reasoning":{"allowed_options":["off","on"],"default":"on"}}},
                {"type":"llm","key":"vision-live-other","capabilities":{"vision":false,"reasoning":{"allowed_options":["high"],"default":"high"}}}
            ]}"#;
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", body).as_bytes())
                .unwrap();
        });
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let mut edit = |message| dialog.update(Message::Profiles(message));
        let _ = edit(ProfileMessage::Add);
        let _ = edit(ProfileMessage::ConnectionMode(ConnectionMode::LmStudio));
        let _ = edit(ProfileMessage::Edit(Field::Endpoint, address));
        let _ = edit(ProfileMessage::Edit(Field::Model, "vision-live".into()));
        let _ = edit(ProfileMessage::Section(Section::Model));
        let scheduled = dialog.update(Message::Page(Page::Profiles));
        let due = output(scheduled, |message| {
            matches!(message, Message::LmStudioMetadataDue(_))
        });
        let pending = dialog.update(due);
        let reply = output(pending, |message| {
            matches!(message, Message::LmStudioMetadataReady(_, Ok(_)))
        });
        let _ = dialog.update(reply.clone());
        assert_eq!(
            dialog
                .profiles
                .lm_studio_capability(dialog.profiles.selected().unwrap())
                .unwrap()
                .vision,
            Some(true)
        );
        let _ = dialog.update(Message::Profiles(ProfileMessage::Reasoning(
            Reasoning::High,
        )));
        assert_eq!(
            dialog.profiles.selected().unwrap().reasoning,
            Reasoning::Default
        );
        let _ = dialog.update(Message::Profiles(ProfileMessage::Reasoning(Reasoning::On)));
        assert_eq!(dialog.profiles.selected().unwrap().reasoning, Reasoning::On);
        let _ = dialog.update(Message::Profiles(ProfileMessage::Edit(
            Field::Endpoint,
            format!(
                "127.0.0.1:{}",
                if endpoint.port() == 1234 { 1235 } else { 1234 }
            ),
        )));
        let _ = dialog.update(Message::Profiles(ProfileMessage::Reasoning(
            Reasoning::Default,
        )));
        let _ = dialog.update(reply);
        assert!(
            dialog
                .profiles
                .lm_studio_capability(dialog.profiles.selected().unwrap())
                .is_none()
        );
        let _ = dialog.update(Message::Profiles(ProfileMessage::Reasoning(Reasoning::On)));
        assert_eq!(
            dialog.profiles.selected().unwrap().reasoning,
            Reasoning::Default
        );
        worker.join().unwrap();
    }

    #[test]
    fn lm_studio_editor_preserves_https_native_mode_and_auto_preference_through_trust() {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        let destination = "https://localhost:1234/api/v1/chat";
        for message in [
            ProfileMessage::Add,
            ProfileMessage::ConnectionMode(ConnectionMode::LmStudio),
            ProfileMessage::Edit(Field::Endpoint, destination.into()),
            ProfileMessage::Edit(Field::Model, "fixture-preference".into()),
            ProfileMessage::ModelMode(ModelMode::Auto),
        ] {
            let _ = dialog.update(Message::Profiles(message));
        }
        let profile = dialog.profiles.selected().unwrap();
        assert_eq!(profile.destination().unwrap(), destination);
        assert!(profile.validate_connection().is_err());

        let _ = dialog.update(Message::Profiles(ProfileMessage::TrustRemote(true)));
        let profile = dialog.profiles.selected().unwrap();
        assert_eq!(profile.connection_mode, ConnectionMode::LmStudio);
        assert_eq!(profile.model, "fixture-preference");
        assert!(profile.ordinary_lm_studio_auto());
        assert!(profile.validate_connection().is_ok());
        assert!(profile.lm_studio_discovery_key().is_some());

        let _ = dialog.update(Message::Profiles(ProfileMessage::Edit(
            Field::Endpoint,
            "https://localhost:1235/api/v1/chat".into(),
        )));
        let profile = dialog.profiles.selected().unwrap();
        assert_eq!(profile.connection_mode, ConnectionMode::LmStudio);
        assert!(profile.validate_connection().is_err());
    }

    #[test]
    fn lm_studio_compatible_editor_discovers_and_tests_the_full_chat_endpoint() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            for _ in 0..3 {
                let mut inventory = accept(&listener);
                assert!(wire_request(&mut inventory, "GET /api/v1/models HTTP/1.1").is_empty());
                inventory.get_mut().write_all(fixture_http_response(
                    "200 OK",
                    "",
                    r#"{"models":[{"type":"llm","key":"fixture-loaded","loaded_instances":[{"id":"slot"}],"capabilities":{"vision":true,"reasoning":{"allowed_options":["on"]}}},{"type":"llm","key":"fixture-preference","loaded_instances":[]}]}"#,
                ).as_bytes()).unwrap();
            }
            let mut chat = accept(&listener);
            let body: serde_json::Value = serde_json::from_slice(&wire_request(
                &mut chat,
                "POST /v1/chat/completions HTTP/1.1",
            ))
            .unwrap();
            assert_eq!(body["model"], "fixture-loaded");
            assert!(body["messages"].is_array());
            assert!(body.get("reasoning").is_none());
            chat.get_mut()
                .write_all(
                    fixture_http_response(
                        "200 OK",
                        "",
                        r#"{"choices":[{"message":{"content":"OK"}}]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            PathBuf::new(),
            Vec::new(),
            true,
            None,
        );
        for message in [
            ProfileMessage::Add,
            ProfileMessage::ConnectionMode(ConnectionMode::LmStudioCompatible),
            ProfileMessage::Edit(
                Field::Endpoint,
                format!("http://{address}/v1/chat/completions"),
            ),
            ProfileMessage::Edit(Field::Model, "fixture-preference".into()),
            ProfileMessage::ModelMode(ModelMode::Auto),
        ] {
            let _ = dialog.update(Message::Profiles(message));
        }
        let _ = dialog.update(Message::Page(Page::Profiles));
        let discover = dialog.update(Message::Profiles(ProfileMessage::Discover));
        let catalog = output(discover, |message| {
            matches!(message, Message::RequestDone(..))
        });
        let _ = dialog.update(catalog);
        let profile = dialog.profiles.selected().unwrap();
        assert_eq!(profile.model, "fixture-preference");
        assert_eq!(
            dialog
                .profiles
                .lm_studio_capability(profile)
                .unwrap()
                .vision,
            Some(true)
        );
        let _ = dialog.update(Message::Profiles(ProfileMessage::Reasoning(Reasoning::On)));
        assert_eq!(
            dialog.profiles.selected().unwrap().reasoning,
            Reasoning::Default
        );

        let test = dialog.update(Message::Profiles(ProfileMessage::Test));
        let resolved = output(test, |message| {
            matches!(message, Message::OrdinaryAutoTestResolved(..))
        });
        let test = dialog.update(resolved);
        let reply = output(test, |message| matches!(message, Message::RequestDone(..)));
        let _ = dialog.update(reply);
        let profile = dialog.profiles.selected().unwrap();
        assert_eq!(profile.model, "fixture-preference");
        assert_eq!(profile.effective_model(), "fixture-loaded");
        assert!(
            dialog
                .notice
                .contains("Model responded successfully (fixture-loaded)"),
            "{}",
            dialog.notice
        );
        worker.join().unwrap();
    }
}
