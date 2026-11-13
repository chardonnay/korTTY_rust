//! A single modularization request bound to its reviewed HTTPS or CLI payload.

use std::borrow::Cow;
use std::time::Instant;

use zeroize::Zeroizing;

use crate::local_ai::{self, Client, Pending, Profile};

use super::super::{MAX_REMOTE_REQUEST_BYTES, source_lines, source_text};
use super::{GenerationRequest, Output, Plan, PlanRequest, Progress, Repair, Snapshot, TIMEOUT};

const NEEDS_MASKING: &str = "The rendered modularization request still contains text that needs masking. Review a separately redacted source or choose a local profile.";
const UNSAFE_RESPONSE: &str =
    "The proposed modularization contained text that needs masking and was discarded.";

type Mask<'a> = dyn Fn(&str) -> Result<String, String> + 'a;

impl Snapshot {
    pub(crate) fn masked_for_modularization(
        &self,
        mut mask: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        self.validate()?;
        let mut masked = self.clone();
        masked.name = mask(&self.name)?;
        for file in &mut masked.files {
            if mask(&file.path)? != file.path {
                return Err(
                    "A source path needs masking. Rename the source or choose a local profile."
                        .into(),
                );
            }
            file.language = mask(&file.language)?;
            let content = mask(&file.content)?;
            if source_lines(&source_text(&content)).count()
                != source_lines(&source_text(&file.content)).count()
            {
                return Err("Masking changed the source line boundaries. Review a separately redacted source or choose a local profile.".into());
            }
            file.content = content;
        }
        masked.validate()?;
        Ok(masked)
    }
}

struct Outbound {
    profile: Profile,
    system: Zeroizing<String>,
    input: Zeroizing<String>,
    cli: Option<Box<local_ai::cli::Request>>,
}

impl Outbound {
    fn new(
        profile: &Profile,
        mut system: String,
        input: String,
        mask: &Mask<'_>,
    ) -> Result<Self, String> {
        if !profile.is_remote() && !profile.is_cli() {
            return Err("Select a reviewed HTTPS or CLI profile.".into());
        }
        if let Some(suffix) = profile.structured_system_suffix() {
            system.push_str(suffix);
        }
        if let Cow::Owned(rendered) = profile.render_system_prompt(&system) {
            system = rendered;
        }
        if system.len().saturating_add(input.len()) > MAX_REMOTE_REQUEST_BYTES {
            return Err("The outbound modularization request exceeds 512 KiB.".into());
        }
        require_masked(mask, [system.as_str(), input.as_str()], NEEDS_MASKING)?;
        let cli = if profile.is_cli() {
            let request = profile
                .prepare_cli_chat(&system, &[], &input)
                .map_err(local_ai::Error::message)?;
            require_masked(mask, request.reviewed_arguments(), NEEDS_MASKING)?;
            require_masked(
                mask,
                std::iter::once(request.primary().1).chain(request.secondary()),
                NEEDS_MASKING,
            )?;
            let command = request.command_review();
            require_masked(mask, [command.as_str()], NEEDS_MASKING)?;
            Some(Box::new(request))
        } else {
            None
        };
        Ok(Self {
            profile: profile.clone(),
            system: Zeroizing::new(system),
            input: Zeroizing::new(input),
            cli,
        })
    }

    fn preview(&self) -> String {
        if let Some(request) = &self.cli {
            let (role, payload) = request.primary();
            let mut preview = format!("{}\n\n{role}\n{payload}", request.command_review());
            if let Some(user) = request.secondary() {
                use std::fmt::Write as _;
                let _ = write!(preview, "\n\nExact UTF-8 user-prompt.txt\n{user}");
            }
            preview
        } else {
            format!(
                "System prompt\n{}\n\nUser prompt\n{}",
                self.system.as_str(),
                self.input.as_str()
            )
        }
    }

    fn start<T: Send + 'static, R>(
        self,
        credential: Option<Zeroizing<String>>,
        guard: impl Fn() -> Result<(), String> + Send + 'static,
        parse: impl FnOnce(&str) -> Result<R, String> + Send + 'static,
        complete: impl FnOnce(R) -> Result<T, String> + Send + 'static,
    ) -> Result<Pending<T>, String> {
        local_ai::start(self.profile, move |client| {
            require_current(client, &guard)?;
            let deadline = Instant::now() + TIMEOUT;
            let answer = if let Some(request) = self.cli {
                client.complete_cli_request_before_guarded(*request, deadline, &guard)
            } else {
                client.complete_reviewed_https_text_before(
                    &self.system,
                    &self.input,
                    deadline,
                    credential,
                    &guard,
                )
            }
            .map(Zeroizing::new)
            .map_err(local_ai::Error::message)?;
            let proposal = parse(&answer)?;
            require_current(client, &guard)?;
            complete(proposal)
        })
    }
}

fn require_current(client: &Client, guard: impl Fn() -> Result<(), String>) -> Result<(), String> {
    guard()?;
    if !client.active() {
        return Err(local_ai::Error::Cancelled.message());
    }
    Ok(())
}

fn require_masked<'a>(
    mask: &Mask<'_>,
    fields: impl IntoIterator<Item = &'a str>,
    message: &str,
) -> Result<(), String> {
    for field in fields {
        if Zeroizing::new(mask(field)?).as_str() != field {
            return Err(message.into());
        }
    }
    Ok(())
}

fn require_plan_masked(mask: &Mask<'_>, plan: &Plan, message: &str) -> Result<(), String> {
    require_masked(
        mask,
        std::iter::once(plan.rationale.as_str()).chain(plan.files.iter().flat_map(|file| {
            [file.path.as_str(), file.purpose.as_str()]
                .into_iter()
                .chain(file.symbols.iter().map(String::as_str))
        })),
        message,
    )
}

pub(crate) struct ReviewedPlan(Outbound);

impl PlanRequest {
    pub(crate) fn review_outbound(
        self,
        profile: &Profile,
        mask: impl Fn(&str) -> Result<String, String>,
    ) -> Result<ReviewedPlan, String> {
        Outbound::new(profile, self.system, self.input, &mask).map(ReviewedPlan)
    }
}

impl ReviewedPlan {
    pub(crate) fn preview(&self) -> String {
        self.0.preview()
    }

    pub(crate) fn system(&self) -> &str {
        &self.0.system
    }

    pub(crate) fn input(&self) -> &str {
        &self.0.input
    }

    pub(crate) fn start<T: Send + 'static>(
        self,
        credential: Option<Zeroizing<String>>,
        guard: impl Fn() -> Result<(), String> + Send + 'static,
        mask: impl Fn(&str) -> Result<String, String> + Send + 'static,
        complete: impl FnOnce(Plan) -> Result<T, String> + Send + 'static,
    ) -> Result<Pending<T>, String> {
        self.0.start(
            credential,
            guard,
            move |answer| {
                let plan: Plan = super::decode(
                    answer,
                    "The response did not contain the requested modularization plan.",
                )?;
                plan.validate()?;
                require_plan_masked(&mask, &plan, UNSAFE_RESPONSE)?;
                Ok(plan)
            },
            complete,
        )
    }
}

pub(crate) enum NextStep {
    Ready(Progress),
    Review(Box<ReviewedStep>),
}

pub(crate) struct ReviewedStep {
    outbound: Outbound,
    plan: Plan,
    progress: Progress,
    path: String,
    repair_index: Option<usize>,
}

impl GenerationRequest {
    pub(crate) fn review_next(
        self,
        mut progress: Progress,
        profile: &Profile,
        mask: impl Fn(&str) -> Result<String, String>,
    ) -> Result<NextStep, String> {
        progress.validate(&self.plan)?;
        let Some(step) = next_file(&mut progress, &self.plan) else {
            return Ok(NextStep::Ready(progress));
        };
        let file = self
            .plan
            .generation_order()
            .nth(step.index)
            .expect("validated modularization step");
        let diagnostics = match &progress.repair {
            Repair::Running { diagnostics, .. } => diagnostics.as_slice(),
            _ => &[],
        };
        // A saved checkpoint can be reopened with a new credential mask. Check
        // decoded fields before JSON escaping changes how secrets are spelled.
        require_plan_masked(&mask, &self.plan, NEEDS_MASKING)?;
        require_masked(
            &mask,
            progress
                .outputs
                .iter()
                .filter(|output| output.path != file.path)
                .flat_map(|output| [output.path.as_str(), output.content.as_str()])
                .chain(diagnostics.iter().map(String::as_str)),
            NEEDS_MASKING,
        )?;
        let input = self.generation_input(file, &progress.outputs, diagnostics)?;
        let outbound = Outbound::new(profile, self.system, input, &mask)?;
        Ok(NextStep::Review(Box::new(ReviewedStep {
            outbound,
            path: file.path.clone(),
            plan: self.plan,
            progress,
            repair_index: step.repair.then_some(step.index),
        })))
    }
}

impl ReviewedStep {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn is_repair(&self) -> bool {
        self.repair_index.is_some()
    }

    pub(crate) fn preview(&self) -> String {
        self.outbound.preview()
    }

    pub(crate) fn system(&self) -> &str {
        &self.outbound.system
    }

    pub(crate) fn input(&self) -> &str {
        &self.outbound.input
    }

    pub(crate) fn start<T: Send + 'static>(
        self,
        credential: Option<Zeroizing<String>>,
        guard: impl Fn() -> Result<(), String> + Send + 'static,
        mask: impl Fn(&str) -> Result<String, String> + Send + 'static,
        complete: impl FnOnce(Progress) -> Result<T, String> + Send + 'static,
    ) -> Result<Pending<T>, String> {
        let Self {
            outbound,
            plan,
            mut progress,
            path,
            repair_index,
        } = self;
        outbound.start(
            credential,
            guard,
            move |answer| {
                let content = super::decode_file(answer, &path)?;
                require_masked(&mask, [content.as_str()], UNSAFE_RESPONSE)?;
                if let Some(index) = repair_index {
                    progress.outputs[index].content = content;
                    let Repair::Running { next, .. } = &mut progress.repair else {
                        unreachable!("reviewed repair step");
                    };
                    *next += 1;
                } else {
                    progress.outputs.push(Output { path, content });
                }
                next_file(&mut progress, &plan);
                progress.validate(&plan)?;
                Ok(progress)
            },
            complete,
        )
    }
}

struct FileStep {
    index: usize,
    repair: bool,
}

fn next_file(progress: &mut Progress, plan: &Plan) -> Option<FileStep> {
    for (index, file) in plan
        .generation_order()
        .enumerate()
        .skip(progress.outputs.len())
    {
        if !file.package_marker() {
            return Some(FileStep {
                index,
                repair: false,
            });
        }
        progress.outputs.push(Output {
            path: file.path.clone(),
            content: String::new(),
        });
    }
    if matches!(progress.repair, Repair::Pending) {
        progress.begin_repair(plan);
    }
    if let Repair::Running { files, next, .. } = &progress.repair {
        if let Some(&index) = files.get(*next) {
            return Some(FileStep {
                index,
                repair: true,
            });
        }
        progress.repair = Repair::Complete;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use crate::ai_manager::CodeTextLanguage;
    use crate::local_ai::test_support::{exclusive_run, listener};
    use crate::local_ai::{ProfileOptions, RemoteProtocol};
    use crate::snippet_analysis::model::modularization::Module;

    use super::*;

    fn mask(input: &str) -> Result<String, String> {
        Ok(input.replace("private-value", "***"))
    }

    fn source() -> Snapshot {
        Snapshot::snippet(
            "entry.py".into(),
            "python".into(),
            "print('private-value')\nprint('safe')\n".into(),
        )
    }

    fn plan() -> Plan {
        Plan {
            recommended: true,
            rationale: "Extract helpers.".into(),
            files: ["entry.py", "pkg/__init__.py", "pkg/helpers.py"]
                .into_iter()
                .enumerate()
                .map(|(index, path)| Module {
                    path: path.into(),
                    purpose: "Reviewed module.".into(),
                    symbols: Vec::new(),
                    executable: index == 0,
                    entry_point: index == 0,
                })
                .collect(),
        }
    }

    #[test]
    fn https_review_checks_masking_and_stops_before_connecting_when_revoked() {
        let (socket, _) = listener();
        let profile = Profile::remote(
            &format!("https://{}", socket.local_addr().unwrap()),
            "fixture",
            RemoteProtocol::Anthropic,
            ProfileOptions::default(),
        )
        .unwrap();
        let raw = PlanRequest::new(&source(), "en", "", "").unwrap();
        assert!(raw.review_outbound(&profile, mask).is_err());
        assert!(
            source()
                .masked_for_modularization(|input| Ok(input.replace('\n', "")))
                .is_err()
        );
        let masked = source().masked_for_modularization(mask).unwrap();
        let review = PlanRequest::new(&masked, "en", "", "")
            .unwrap()
            .review_outbound(&profile, mask)
            .unwrap();
        assert!(
            review
                .system()
                .ends_with(profile.structured_system_suffix().unwrap())
        );
        assert!(review.input().contains("1 | print('***')"));
        assert!(review.input().contains("2 | print('safe')"));
        assert!(!review.preview().contains("private-value"));
        assert_eq!(socket.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let _exclusive = exclusive_run();
        let guard_calls = AtomicUsize::new(0);
        let pending = review
            .start(
                None,
                move || {
                    if guard_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                        Ok(())
                    } else {
                        Err("The reviewed profile changed.".into())
                    }
                },
                mask,
                |_| panic!("revoked result must not be persisted"),
            )
            .unwrap();
        let result: Result<(), String> = pending.reply.recv_blocking().unwrap();
        assert_eq!(result.unwrap_err(), "The reviewed profile changed.");
        assert_eq!(socket.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn resumed_generation_rejects_json_escaped_secrets_in_retained_fields() {
        let profile = Profile::remote(
            "https://localhost:1",
            "fixture",
            RemoteProtocol::OpenAi,
            ProfileOptions::default(),
        )
        .unwrap();
        let secret = r#"sample"private\value"#;
        let mask = |input: &str| Ok(input.replace(secret, "***"));
        for (plan_secret, output_secret) in [(true, false), (false, true)] {
            let mut plan = plan();
            if plan_secret {
                plan.files[0].purpose = format!("Contains {secret}.");
            }
            let progress = Progress {
                outputs: vec![
                    Output {
                        path: "pkg/__init__.py".into(),
                        content: String::new(),
                    },
                    Output {
                        path: "pkg/helpers.py".into(),
                        content: if output_secret {
                            format!("VALUE = {secret}\n")
                        } else {
                            "VALUE = 1\n".into()
                        },
                    },
                ],
                ..Progress::default()
            };
            let request =
                GenerationRequest::new(&source(), plan, CodeTextLanguage::English, "", "").unwrap();
            assert!(!request.preview(&progress).unwrap().contains(secret));
            assert!(
                matches!(request.review_next(progress, &profile, mask), Err(reason) if reason == NEEDS_MASKING)
            );
        }
    }

    #[test]
    fn cli_review_rejects_a_secret_in_the_raw_argument_before_json_escaping() {
        #[cfg(unix)]
        let executable = std::path::PathBuf::from("/fixture/tool");
        #[cfg(windows)]
        let executable = std::path::PathBuf::from(r"C:\fixture\tool.exe");
        let secret = r#"sample"private\value"#;
        let program = local_ai::cli::Program::approved(
            executable.clone(),
            &[secret.into(), "{stdinPrompt}".into()],
            "",
            [1; 32],
        )
        .unwrap();
        assert!(!program.command_review().contains(secret));
        let profile = Profile::configured_cli(
            program,
            executable.parent().unwrap().to_owned(),
            "fixture",
            None,
        );
        let request = PlanRequest::new(&source(), "en", "", "").unwrap();
        assert!(
            matches!(request.review_outbound(&profile, |input| Ok(input.replace(secret, "***"))), Err(reason) if reason == NEEDS_MASKING)
        );
    }

    #[test]
    fn cli_generation_runs_only_the_reviewed_file_and_repairs_only_after_new_review() {
        let _exclusive = exclusive_run();
        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let executable = local_ai::cli::fixture_executable(&root);
        let (executable, digest) = local_ai::cli::Program::inspect(&executable).unwrap();
        let transport = |response: String| {
            let args = [
                "fixture_scheduled_proposal".into(),
                response,
                "{promptFile}".into(),
            ];
            let program =
                local_ai::cli::Program::approved(executable.clone(), &args, "", digest).unwrap();
            Profile::configured_cli(program, root.clone(), "fixture", None)
        };
        let masked = source().masked_for_modularization(mask).unwrap();
        let review = |progress, response: String| {
            let request =
                GenerationRequest::new(&masked, plan(), CodeTextLanguage::English, "", "").unwrap();
            let NextStep::Review(step) = request
                .review_next(progress, &transport(response), mask)
                .unwrap()
            else {
                panic!("the next file must have its own review");
            };
            step
        };

        let run = |step: ReviewedStep| {
            let preview = step.preview();
            let (command, prompt) = preview.split_once("\n\nExact UTF-8 prompt.txt\n").unwrap();
            let (_, args) = command.rsplit_once("\nArguments: ").unwrap();
            let args: Vec<String> = serde_json::from_str(args).unwrap();
            let reviewed_path = std::path::PathBuf::from(args.last().unwrap());
            assert!(!reviewed_path.exists());
            let capture = root.join("fixture-scheduled-prompt");
            assert!(!capture.exists());
            let pending = step.start(None, || Ok(()), mask, Ok).unwrap();
            let progress = pending.reply.recv_blocking().unwrap().unwrap();
            assert_eq!(std::fs::read_to_string(&capture).unwrap(), prompt);
            std::fs::remove_file(capture).unwrap();
            assert!(!reviewed_path.exists());
            progress
        };
        let first = review(
            Progress::default(),
            json!({"lines":["VALUE = 1"]}).to_string(),
        );
        assert_eq!(first.path(), "pkg/helpers.py");
        assert!(!first.is_repair());
        assert!(!first.preview().contains("private-value"));
        let progress = run(*first);
        assert_eq!(progress.outputs[0].path, "pkg/__init__.py");
        assert!(progress.outputs[0].content.is_empty());
        assert_eq!(progress.outputs[1].content, "VALUE = 1\n");
        assert!(!progress.ready());

        let step = review(progress, json!({"lines":["print('ready')"]}).to_string());
        assert_eq!(step.path(), "entry.py");
        assert!(step.input().contains("VALUE = 1"));
        let progress = run(*step);
        assert_eq!(progress.outputs.len(), 3);
        assert!(!progress.ready());

        let step = review(progress.clone(), json!({"lines":["VALUE = 2"]}).to_string());
        let guard_calls = AtomicUsize::new(0);
        let pending = step
            .start(
                None,
                move || {
                    if guard_calls.fetch_add(1, Ordering::Relaxed) < 3 {
                        Ok(())
                    } else {
                        Err("The reviewed authority changed after dispatch.".into())
                    }
                },
                mask,
                |_| -> Result<(), String> { panic!("a revoked proposal must not be persisted") },
            )
            .unwrap();
        assert_eq!(
            pending.reply.recv_blocking().unwrap().unwrap_err(),
            "The reviewed authority changed after dispatch."
        );
        let capture = root.join("fixture-scheduled-prompt");
        assert!(capture.is_file());
        std::fs::remove_file(&capture).unwrap();

        let step = review(
            progress,
            // The response conceals the known secret in a JSON escape.
            r#"{"lines":["VALUE = 'private\u002dvalue'"]}"#.into(),
        );
        assert!(step.is_repair());
        assert_eq!(step.path(), "pkg/helpers.py");
        assert!(
            step.input()
                .contains("No proposed file refers to pkg/helpers.py.")
        );
        let pending = step
            .start(
                None,
                || Ok(()),
                mask,
                |_| -> Result<(), String> {
                    panic!("an unsafe decoded file must not be persisted")
                },
            )
            .unwrap();
        assert_eq!(
            pending.reply.recv_blocking().unwrap().unwrap_err(),
            UNSAFE_RESPONSE
        );
    }
}
