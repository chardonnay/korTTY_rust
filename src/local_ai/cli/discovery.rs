use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{Error, Program, Request, execute, execute_capture, require_policy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelList {
    Loaded,
    Downloaded,
}

impl ModelList {
    fn arguments(self) -> [&'static str; 2] {
        [
            match self {
                Self::Loaded => "ps",
                Self::Downloaded => "ls",
            },
            "--json",
        ]
    }
}

pub(crate) struct Discovery {
    program: Program,
    root: PathBuf,
    request: Request,
    list: ModelList,
}

impl Discovery {
    pub(crate) fn inspect(path: &Path, root: PathBuf, list: ModelList) -> Result<Self, String> {
        let (path, fingerprint) = Program::inspect(path)?;
        let arguments = list.arguments().map(str::to_owned);
        let program = Program::approved(path, &arguments, "", fingerprint)?;
        let request = Request::without_input(&program, &root).map_err(Error::message)?;
        Ok(Self {
            program,
            root,
            request,
            list,
        })
    }

    pub(crate) fn command_review(&self) -> String {
        self.request.command_review()
    }

    pub(crate) fn run(&self, canceled: &AtomicBool) -> Result<Discovered, String> {
        self.run_checked(canceled, || require_policy(false).map_err(Error::message))
            .map_err(Error::message)
    }

    fn run_checked(
        &self,
        canceled: &AtomicBool,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<Discovered, Error> {
        #[cfg(target_os = "macos")]
        let _power = kortty_macos::AppNapActivity::begin();
        let output = execute(
            &self.program,
            &self.root,
            self.request.clone(),
            Instant::now() + Duration::from_secs(30),
            canceled,
            before_dispatch,
        )?;
        let keys = parse(&output)?;
        Ok(Discovered {
            keys,
            loader: (self.list == ModelList::Downloaded).then(|| Loader {
                path: self.program.path.clone(),
                fingerprint: self.program.fingerprint,
                root: self.root.clone(),
            }),
        })
    }
}

pub(crate) struct Discovered {
    keys: Vec<String>,
    loader: Option<Loader>,
}

struct Loader {
    path: PathBuf,
    fingerprint: [u8; 32],
    root: PathBuf,
}

impl Discovered {
    pub(crate) fn keys(&self) -> &[String] {
        &self.keys
    }

    pub(crate) fn prepare_load(&self, model: &str) -> Result<Load, String> {
        let source = self
            .loader
            .as_ref()
            .filter(|_| valid_key(model) && self.keys.iter().any(|key| key == model))
            .ok_or("Choose a model from the reviewed downloaded LM Studio list.")?;
        let arguments = ["load".into(), model.to_owned(), "-y".into()];
        let program = Program::approved(source.path.clone(), &arguments, "", source.fingerprint)?;
        let request = Request::without_input(&program, &source.root).map_err(Error::message)?;
        Ok(Load {
            program,
            root: source.root.clone(),
            request,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        keys: Vec<String>,
        path: PathBuf,
        root: PathBuf,
        list: ModelList,
    ) -> Self {
        Self {
            keys,
            loader: (list == ModelList::Downloaded).then_some(Loader {
                path,
                root,
                fingerprint: [1; 32],
            }),
        }
    }
}

pub(crate) struct Load {
    program: Program,
    root: PathBuf,
    request: Request,
}

impl Load {
    pub(crate) fn command_review(&self) -> String {
        self.request.command_review()
    }

    pub(crate) fn run(&self, canceled: &AtomicBool) -> Result<(), String> {
        self.run_checked(canceled, || require_policy(false).map_err(Error::message))
            .map_err(Error::message)
    }

    fn run_checked(
        &self,
        canceled: &AtomicBool,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<(), Error> {
        // A successful load can have no stdout. The shared runner still bounds its output
        // and checks the executable, cancellation, and the process exit status.
        #[cfg(target_os = "macos")]
        let _power = kortty_macos::AppNapActivity::begin();
        execute_capture(
            &self.program,
            &self.root,
            self.request.clone(),
            Instant::now() + Duration::from_secs(180),
            canceled,
            super::Capture::Answer,
            before_dispatch,
        )
        .map(drop)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    #[serde(rename = "type")]
    kind: Option<String>,
    model_key: Option<String>,
}

fn parse(output: &str) -> Result<Vec<String>, Error> {
    // Some lms versions wake their local service before printing the JSON list.
    let output = output.trim();
    let output = output
        .strip_prefix("Waking up LM Studio service...")
        .and_then(|rest| rest.strip_prefix(['\r', '\n']))
        .unwrap_or(output);
    let models: Vec<Model> = serde_json::from_str(output)
        .map_err(|_| Error::Invalid("LM Studio did not return a JSON model list"))?;
    let mut seen = HashSet::new();
    Ok(models
        .into_iter()
        .filter(|model| {
            model
                .kind
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("llm"))
        })
        .filter_map(|model| model.model_key)
        .filter(|key| valid_key(key) && seen.insert(key.clone()))
        .collect())
}

fn valid_key(key: &str) -> bool {
    crate::local_ai::valid_model(key)
        && !key.starts_with('-')
        && key
            .chars()
            .all(crate::session_journal::is_display_character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lm_studio_discovery_runs_only_reviewed_list_commands_with_empty_stdin() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let path = super::super::fixture_executable(&root);
        let canceled = AtomicBool::new(false);
        for (list, expected_argv, expected) in [
            (
                ModelList::Loaded,
                r#"["ps","--json"]"#,
                &["qwen/qwen3.6-35b-a3b"][..],
            ),
            (
                ModelList::Downloaded,
                r#"["ls","--json"]"#,
                &["qwen/qwen3.6-35b-a3b", "openai/gpt-oss-20b"][..],
            ),
        ] {
            let reviewed = Discovery::inspect(&path, root.clone(), list).unwrap();
            assert!(reviewed.command_review().ends_with(expected_argv));
            let result = reviewed.run_checked(&canceled, || Err("revoked".into()));
            assert!(matches!(result, Err(Error::Review(message)) if message == "revoked"));
            assert!(!root.join("fixture-discovery-started").exists());
            let result = reviewed.run_checked(&canceled, || Ok(())).unwrap();
            assert_eq!(result.keys(), expected);
            assert!(root.join("fixture-discovery-started").is_file());
            std::fs::remove_file(root.join("fixture-discovery-started")).unwrap();
        }
    }

    #[test]
    fn lm_studio_load_requires_a_reviewed_download_and_keeps_the_exact_command() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let path = super::super::fixture_executable(&root);
        let canceled = AtomicBool::new(false);
        let models = |list| {
            Discovery::inspect(&path, root.clone(), list)
                .unwrap()
                .run_checked(&canceled, || Ok(()))
                .unwrap()
        };
        let selected = "openai/gpt-oss-20b";
        assert!(
            models(ModelList::Loaded)
                .prepare_load("qwen/qwen3.6-35b-a3b")
                .is_err()
        );
        let downloaded = models(ModelList::Downloaded);
        assert!(downloaded.prepare_load("--help").is_err());
        assert!(downloaded.prepare_load("arbitrary/model").is_err());
        let load = downloaded.prepare_load(selected).unwrap();
        assert!(
            load.command_review()
                .ends_with(r#"["load","openai/gpt-oss-20b","-y"]"#)
        );
        let marker = root.join("fixture-loaded-model");
        assert!(
            matches!(load.run_checked(&canceled, || Err("revoked".into())), Err(Error::Review(reason)) if reason == "revoked")
        );
        assert!(!marker.exists());
        let canceled_early = AtomicBool::new(true);
        assert!(matches!(
            load.run_checked(&canceled_early, || Ok(())),
            Err(Error::Cancelled)
        ));
        assert!(!marker.exists());
        load.run_checked(&canceled, || Ok(())).unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), selected);
        std::fs::remove_file(&marker).unwrap();
        // A failed load must be reported even when stderr contains provider output.
        assert!(
            downloaded
                .prepare_load("qwen/qwen3.6-35b-a3b")
                .unwrap()
                .run_checked(&canceled, || Ok(()))
                .is_err()
        );
        assert!(!marker.exists());
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            load.run_checked(&canceled, || Ok(())),
            Err(Error::Review(_))
        ));
        assert!(!marker.exists());
    }

    #[test]
    fn lm_studio_discovery_rejects_invalid_json_and_unsafe_or_duplicate_keys() {
        assert!(parse("[]").unwrap().is_empty());
        assert!(parse("log\n[]").is_err());
        assert!(parse("[]\nextra output").is_err());
        let result = parse(
            r#"[{"type":"llm","modelKey":"--host"},{"type":"llm","modelKey":"two words"},
                 {"type":"LLM","modelKey":"same"},{"type":"llm","modelKey":"same"}]"#,
        )
        .unwrap();
        assert_eq!(result, ["same"]);
    }
}
