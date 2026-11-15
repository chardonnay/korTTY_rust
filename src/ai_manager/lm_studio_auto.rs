//! Process-local resolution of saved LM Studio automatic profiles.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::local_ai::{self, LmStudioAutoModel, Pending};

use super::{ProfileId, profiles};

pub(crate) const POLL: Duration = Duration::from_secs(5);
const REFRESH_AFTER: Duration = Duration::from_secs(30);
const CAPACITY: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selection {
    pub(super) root: PathBuf,
    pub(super) id: ProfileId,
    pub(super) settings: profiles::LmStudioAutoSettings,
}

impl Selection {
    pub(crate) fn resolve(&self) -> Result<Pending<LmStudioAutoModel>, String> {
        let settings = &self.settings;
        local_ai::resolve_lm_studio_auto(
            &settings.endpoint,
            &settings.preferred,
            settings.credential_env.clone(),
            settings.mode.uses_native_lm_studio(),
        )
    }

    fn accepts(&self, model: &LmStudioAutoModel) -> bool {
        let settings = &self.settings;
        model.matches(
            &settings.endpoint,
            &settings.preferred,
            settings.credential_env.as_deref(),
            settings.mode.uses_native_lm_studio(),
        )
    }
}

struct Entry {
    selection: Selection,
    started: Instant,
    completed: Instant,
    result: Result<LmStudioAutoModel, String>,
}

static RESOLUTIONS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

pub(super) fn cached(selection: &Selection) -> Option<Result<LmStudioAutoModel, String>> {
    RESOLUTIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|entry| entry.selection == *selection)
        .filter(|entry| {
            entry
                .result
                .as_ref()
                .ok()
                .is_none_or(|model| selection.accepts(model))
        })
        .map(|entry| entry.result.clone())
}

pub(crate) fn refresh_order(
    selection: &Selection,
    forced: Option<Instant>,
) -> Option<Option<Instant>> {
    let values = RESOLUTIONS.lock().unwrap_or_else(PoisonError::into_inner);
    match values.iter().find(|entry| entry.selection == *selection) {
        None => Some(None),
        Some(entry)
            if entry.completed.elapsed() >= REFRESH_AFTER
                || forced.is_some_and(|since| entry.started < since)
                || entry
                    .result
                    .as_ref()
                    .is_ok_and(|model| !selection.accepts(model)) =>
        {
            Some(Some(entry.completed))
        }
        _ => None,
    }
}

pub(crate) fn publish(
    selection: Selection,
    started: Instant,
    result: Result<LmStudioAutoModel, String>,
) -> bool {
    if result.as_ref().is_ok_and(|model| !selection.accepts(model)) {
        return false;
    }
    let mut values = RESOLUTIONS.lock().unwrap_or_else(PoisonError::into_inner);
    if values
        .iter()
        .any(|entry| entry.selection == selection && entry.started > started)
    {
        return false;
    }
    values.retain(|entry| entry.selection != selection);
    if values.len() == CAPACITY {
        values.remove(0);
    }
    values.push(Entry {
        selection,
        started,
        completed: Instant::now(),
        result,
    });
    true
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::ai_manager::{Configuration, FILE_NAME, ProfileRef};
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, listener, wire_request,
    };

    #[test]
    fn lm_studio_auto_projection_reloads_the_exact_key_and_changes_the_review_revision() {
        verify_auto_projection("lm-studio", "/api/v1/chat", false);
    }

    #[test]
    fn lm_studio_compatible_auto_projection_retains_its_chat_route() {
        verify_auto_projection("lm-studio-compatible", "/v1/chat/completions", false);
    }

    #[test]
    fn lm_studio_localhost_auto_projection_keeps_the_host_and_review_binding() {
        verify_auto_projection("lm-studio", "/api/v1/chat", true);
    }

    fn verify_auto_projection(mode: &str, chat_path: &str, localhost: bool) {
        let _exclusive = exclusive_run();
        let (server, _) = listener();
        let endpoint = server.local_addr().unwrap();
        let (stored_endpoint, origin) = if localhost {
            let origin = format!("http://localhost:{}", endpoint.port());
            (origin.clone(), origin)
        } else {
            (endpoint.to_string(), format!("http://{endpoint}"))
        };
        let worker = std::thread::spawn(move || {
            for key in ["loaded-first", "loaded-second"] {
                let mut stream = accept(&server);
                assert!(wire_request(&mut stream, "GET /api/v1/models HTTP/1.1").is_empty());
                let body = serde_json::json!({"models":[
                    {"type":"llm","key":"stored-preference","loaded_instances":[]},
                    {"type":"llm","key":key,"loaded_instances":[{"id":"slot"}]}
                ]})
                .to_string();
                stream
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &body).as_bytes())
                    .unwrap();
            }
        });
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let preferences = serde_json::json!({"profiles": {
            "profiles":[{"id":1,"name":"Auto fixture","connection_mode":mode,
                "model_mode":"auto","endpoint":stored_endpoint,"model":"stored-preference"}],
            "default":1,"last_id":1
        }});
        let draft = Configuration::decode(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        draft
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_from(&root).unwrap();
        let file = std::fs::read(root.join(FILE_NAME)).unwrap();
        let reference = ProfileRef::Numeric(1);
        assert!(saved.ai_model_resolution_pending(1));
        assert!(saved.agent_profile_reference_revision(&reference).is_none());
        let selection = saved.lm_studio_auto_selections().next().unwrap();
        let apply = || {
            let started = Instant::now();
            let pending = selection.resolve().unwrap();
            let model = pending.reply.recv_blocking().unwrap().unwrap();
            assert!(publish(selection.clone(), started, Ok(model)));
            saved.reload_saved().unwrap()
        };
        let first = apply();
        let (_, _, model, destination) = first.agent_profiles().next().unwrap();
        assert_eq!(model, "loaded-first");
        assert_eq!(destination, format!("{origin}{chat_path}"));
        let first_revision = first.agent_profile_reference_revision(&reference).unwrap();
        let second = apply();
        assert!(!second.ai_model_resolution_pending(1));
        assert!(second.lm_studio_auto_profiles_changed(&first));
        assert!(!second.local_ai_profile_matches(1, "loaded-first", &destination));
        assert_eq!(
            second
                .agent_transport_for_reference(&reference)
                .unwrap()
                .0
                .model(),
            "loaded-second"
        );
        assert_ne!(
            second.agent_profile_reference_revision(&reference).unwrap(),
            first_revision
        );
        assert_eq!(second, first);
        assert_eq!(std::fs::read(root.join(FILE_NAME)).unwrap(), file);
        worker.join().unwrap();
    }
}
