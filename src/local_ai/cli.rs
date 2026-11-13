//! A reviewed, one-shot native CLI with bounded UTF-8 input and stdout.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use regex::Regex;
use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::{Directory, executable, nonce};

use super::{Error, StreamDelta, reply};

pub(crate) mod catalog;
mod discovery;
mod input;
mod minimax;
pub(crate) use discovery::{Discovered, Discovery, Load, ModelList};
pub(crate) use input::Request;
use input::{Argument, FileRole, Mode};
pub(crate) use minimax::{MiniMaxRegion, valid_minimax_config_path, valid_minimax_module_path};

pub(crate) const MAX_ARGS: usize = 24;
pub(crate) const MAX_ARG_BYTES: usize = 8 * 1024;
pub(crate) const MODEL: &str = "{model}";

pub(crate) fn display_model(model: &str) -> &str {
    if model.is_empty() {
        "CLI default"
    } else {
        model
    }
}
const LM_STUDIO_PROMPT: &str = "Use the conversation provided on standard input as the complete prompt. Follow its instructions exactly and reply with only the requested answer.";
pub(crate) const LM_STUDIO_STDIN: &[&str] =
    &["chat", MODEL, "-p", LM_STUDIO_PROMPT, "{stdinPrompt}"];
// With piped stdin, lms uses a loaded model or exits before prompting for one.
pub(crate) const LM_STUDIO_LOADED_STDIN: &[&str] =
    &["chat", "-p", LM_STUDIO_PROMPT, "{stdinPrompt}"];
pub(crate) const CODEX_STDIN: &[&str] = &[
    "exec",
    "--sandbox=read-only",
    "--skip-git-repo-check",
    "--ephemeral",
    "--ignore-user-config",
    "--config=approval_policy=\"never\"",
    "--config=model_provider=\"openai\"",
    "--config=openai_base_url=\"https://api.openai.com/v1\"",
    "--config=chatgpt_base_url=\"https://chatgpt.com/backend-api/\"",
    "--config=web_search=\"disabled\"",
    "--config=features.shell_tool=false",
    "--config=features.shell_snapshot=false",
    "--config=features.view_image=false",
    "--config=features.multi_agent=false",
    "--config=features.plugins=false",
    "--config=features.apps=false",
    "--config=mcp_servers={}",
    "-",
    "{stdinPrompt}",
];
const MAX_INPUT: usize = 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_DIAGNOSTICS: usize = 8 * 1024;
const MAX_DURATION: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_millis(15);

#[derive(Clone, Copy)]
enum Capture {
    Answer,
    Password,
    Editor { limit: usize },
}

impl Capture {
    fn output_limit(self) -> usize {
        match self {
            Self::Answer | Self::Password => MAX_OUTPUT,
            Self::Editor { limit } => limit,
        }
    }

    fn diagnostics(self) -> bool {
        matches!(self, Self::Editor { .. })
    }

    fn discard_stdout_tail(self) -> bool {
        matches!(self, Self::Password)
    }

    fn overflow(self) -> Error {
        Error::Invalid(match self {
            Self::Answer => "CLI stdout exceeds 64 KiB",
            Self::Password => "Password program output exceeds 64 KiB.",
            Self::Editor { .. } => "The tool output exceeds the editor size limit.",
        })
    }
}

pub(crate) fn password_program(
    path: PathBuf,
    arguments: &[String],
    fingerprint: [u8; 32],
) -> Result<Program, String> {
    if !executable::valid_path(&path)
        || arguments.len() > MAX_ARGS
        || arguments.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES
        || arguments
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
    {
        return Err("Enter an absolute native executable and bounded literal arguments.".into());
    }
    Ok(Program {
        path,
        arguments: arguments.iter().cloned().map(Argument::Literal).collect(),
        mode: Mode::Stdin,
        fingerprint,
        codex_preset: false,
        minimax: None,
    })
}

pub(crate) fn run_password_program(
    program: &Program,
    root: &Path,
    canceled: &AtomicBool,
    authorize: impl Fn() -> Result<(), String>,
) -> Result<kortty_vault::Secret, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let request = Request::without_input(program, root).map_err(password_error)?;
    let output = execute_capture(
        program,
        root,
        request,
        deadline,
        canceled,
        Capture::Password,
        authorize,
    )
    .map_err(password_error)?;
    let bytes = output.stdout.as_slice();
    if bytes.len() > MAX_OUTPUT {
        let end = bytes[..MAX_OUTPUT]
            .iter()
            .position(|byte| matches!(*byte, b'\r' | b'\n'))
            .ok_or_else(|| "Password program output exceeds 64 KiB.".to_owned())?;
        return parse_password_output(&bytes[..=end]);
    }
    parse_password_output(bytes)
}

#[cfg(test)]
#[test]
fn ssh_password_command_runs_exact_native_argv_and_returns_only_first_line() {
    let scratch = crate::snippets::tests::Profile::new();
    let path = fixture_executable(&scratch.0);
    let (path, fingerprint) = Program::inspect(&path).unwrap();
    let arguments = ["fixture_ssh_password", "{model}", "with a space", "&&"].map(str::to_owned);
    let program = password_program(path, &arguments, fingerprint).unwrap();
    let cancel = AtomicBool::new(false);
    let started = scratch.0.join("fixture-password-started");
    assert!(run_password_program(&program, &scratch.0, &cancel, || Err("revoked".into())).is_err());
    assert!(!started.exists());
    let secret = run_password_program(&program, &scratch.0, &cancel, || Ok(())).unwrap();
    assert_eq!(secret.as_str(), "fixture-password");
    assert!(started.exists());
    assert!(parse_password_output(b"a\0b\n").is_err());
    assert!(parse_password_output(&[0xff]).is_err());
    assert!(parse_password_output(&[b'x'; 1025]).is_err());
}

fn parse_password_output(bytes: &[u8]) -> Result<kortty_vault::Secret, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Password program output is not UTF-8.")?;
    let password = text
        .trim_matches(|character| character <= '\u{20}')
        .split([
            '\r', '\n', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
        ])
        .next()
        .unwrap_or_default();
    if password.is_empty() || password.len() > 1024 || bytes.contains(&0) {
        return Err(
            "The password program must return a nonempty first line of at most 1024 bytes.".into(),
        );
    }
    Ok(kortty_vault::Secret::new(password.to_owned()))
}

fn password_error(error: Error) -> String {
    match error {
        Error::Cancelled => "The password program was cancelled.".into(),
        Error::TimedOut => "The password program timed out.".into(),
        Error::Invalid(reason) => reason.into(),
        _ => "The approved password program could not finish. Review the credential and try again."
            .into(),
    }
}

struct ProcessInput<'a> {
    stdin: Zeroizing<String>,
    deadline: Instant,
    canceled: &'a AtomicBool,
    capture: Capture,
}

#[derive(Default)]
struct ProcessOutput {
    stdout: Zeroizing<Vec<u8>>,
    diagnostics: Zeroizing<Vec<u8>>,
    success: bool,
    credential: Option<Zeroizing<String>>,
}

pub(crate) struct EditorOutput {
    pub(crate) stdout: String,
    pub(crate) diagnostics: String,
    pub(crate) success: bool,
}

pub(crate) const EDITOR_SCRIPT: &str = "formatter-launcher";

pub(crate) fn run_editor_tool(
    program: &Program,
    root: &Path,
    source: &str,
    script: Option<&str>,
    output_limit: usize,
    canceled: &AtomicBool,
    before_dispatch: impl Fn() -> Result<(), String>,
) -> Result<EditorOutput, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let request = Request::editor(program, root, source, script).map_err(editor_error)?;
    let mut output = execute_capture(
        program,
        root,
        request,
        deadline,
        canceled,
        Capture::Editor {
            limit: output_limit,
        },
        before_dispatch,
    )
    .map_err(editor_error)?;
    let stdout = String::from_utf8(std::mem::take(&mut *output.stdout))
        .map_err(|_| "The editor tool returned non-UTF-8 output.")?;
    let diagnostics = clean(&String::from_utf8_lossy(&output.diagnostics)).to_string();
    Ok(EditorOutput {
        stdout,
        diagnostics,
        success: output.success,
    })
}

fn editor_error(error: Error) -> String {
    match error {
        Error::Cancelled => "The editor tool was cancelled.".into(),
        Error::TimedOut => "The editor tool timed out.".into(),
        Error::Invalid(reason) => reason.into(),
        Error::Review(reason) => {
            reason.replace(" Review it again in AI Manager.", " Review it again.")
        }
        _ => "Could not run the approved editor tool.".into(),
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Program {
    path: PathBuf,
    arguments: Vec<Argument>,
    mode: Mode,
    fingerprint: [u8; 32],
    codex_preset: bool,
    minimax: Option<Box<minimax::MiniMax>>,
}

impl Program {
    pub(crate) fn inspect(path: &Path) -> Result<(PathBuf, [u8; 32]), String> {
        Self::inspect_cancellable(path, None)
    }

    pub(crate) fn inspect_cancellable(
        path: &Path,
        canceled: Option<&AtomicBool>,
    ) -> Result<(PathBuf, [u8; 32]), String> {
        if !executable::valid_path(path) {
            return Err("Enter an absolute native executable path.".into());
        }
        let inspect = || {
            let failure = |_| "Could not review the selected native CLI executable.";
            let path = path.canonicalize().map_err(failure)?;
            let mut opened = executable::open(&path).map_err(failure)?;
            let size = opened.file.metadata().map_err(failure)?.len();
            if !(1..=executable::MAX_EXECUTABLE).contains(&size) {
                return Err("The CLI executable must be nonempty and at most 512 MiB.");
            }
            executable::require_native_image(&mut opened.file).map_err(failure)?;
            let fingerprint = executable::digest(&mut opened.file, canceled).map_err(failure)?;
            opened.recheck(&path).map_err(failure)?;
            Ok((path, fingerprint))
        };
        inspect().map_err(str::to_owned)
    }

    pub(crate) fn approved(
        path: PathBuf,
        arguments: &[String],
        model: &str,
        fingerprint: [u8; 32],
    ) -> Result<Self, String> {
        if !executable::valid_path(&path) || !valid_arguments(arguments) {
            return Err("Enter an absolute native executable and bounded arguments.".into());
        }
        let mode = input::mode(arguments).expect("validated prompt placeholders");
        if arguments.iter().any(|argument| argument == MODEL)
            && (!super::valid_model(model) || model.starts_with('-'))
        {
            return Err("Enter a model identifier for the standalone {model} argument.".into());
        }
        let codex_preset = arguments
            .iter()
            .map(String::as_str)
            .eq(CODEX_STDIN.iter().copied());
        let arguments = arguments
            .iter()
            .filter_map(|argument| {
                Some(match argument.as_str() {
                    "{stdinPrompt}" => return None,
                    "{promptFile}" => Argument::File(FileRole::Combined),
                    "{systemPromptFile}" => Argument::File(FileRole::System),
                    "{userPromptFile}" => Argument::File(FileRole::User),
                    MODEL => Argument::Literal(model.into()),
                    _ => Argument::Literal(argument.clone()),
                })
            })
            .collect::<Vec<_>>();
        if arguments
            .iter()
            .map(|argument| argument.review().len())
            .sum::<usize>()
            > MAX_ARG_BYTES
        {
            return Err("Expanded CLI arguments exceed 8 KiB.".into());
        }
        Ok(Self {
            path,
            arguments,
            mode,
            fingerprint,
            codex_preset,
            minimax: None,
        })
    }

    pub(crate) fn destination(&self) -> String {
        if let Some(minimax) = &self.minimax {
            format!("MiniMax CLI · {}", minimax.destination())
        } else if self.codex_preset {
            format!(
                "Local CLI · {} → OpenAI (api.openai.com or chatgpt.com)",
                self.path.display()
            )
        } else {
            format!("Local CLI · {}", self.path.display())
        }
    }

    pub(crate) fn command_review(&self) -> String {
        self.review_arguments(
            &self
                .arguments
                .iter()
                .map(Argument::review)
                .collect::<Vec<_>>(),
        )
    }

    fn review_arguments(&self, arguments: &impl serde::Serialize) -> String {
        use std::fmt::Write as _;
        let mut digest = String::new();
        for byte in self.fingerprint {
            write!(digest, "{byte:02x}").expect("writing to a string");
        }
        if let Some(minimax) = &self.minimax {
            return format!(
                "Node.js executable: {}\nSHA-256: {digest}{}",
                self.path.display(),
                minimax.review(),
            );
        }
        let arguments = serde_json::to_string(arguments).expect("strings serialize");
        format!(
            "Executable: {}\nSHA-256: {digest}\nArguments: {arguments}",
            self.path.display()
        )
    }
}

pub(crate) fn valid_arguments(arguments: &[String]) -> bool {
    arguments.len() <= MAX_ARGS
        && arguments.iter().map(String::len).sum::<usize>() <= MAX_ARG_BYTES
        && arguments
            .iter()
            .all(|argument| !argument.chars().any(char::is_control))
        && input::mode(arguments).is_some()
}

pub(super) fn prompt(
    system: &str,
    history: &[(&str, &str)],
    input: &str,
) -> Result<Zeroizing<String>, Error> {
    use std::fmt::Write as _;
    let mut prompt = Zeroizing::new(format!("System prompt:\n{system}\n\n"));
    if !history.is_empty() {
        prompt.push_str("Conversation history (ordered user and assistant pairs):\n");
        let history = Zeroizing::new(serde_json::to_string(history).map_err(|_| Error::Service)?);
        prompt.push_str(&history);
        prompt.push_str("\n\n");
    }
    write!(prompt, "User prompt:\n{input}\n").expect("writing to a string");
    if prompt.len() > MAX_INPUT {
        return Err(Error::Invalid("the CLI request exceeds 1 MiB"));
    }
    Ok(prompt)
}

pub(crate) fn allowed(policy: &ClipboardPolicy, chat: bool) -> bool {
    policy.ai_allowed
        && (!chat || policy.ai_chat_allowed)
        && policy.minimum_isolation == crate::session_security::IsolationLevel::None
}

pub(super) fn require_policy(chat: bool) -> Result<(), Error> {
    if allowed(&ClipboardPolicy::load(), chat) {
        Ok(())
    } else {
        Err(Error::Review(
            "Managed policy blocks the configured CLI.".into(),
        ))
    }
}

pub(super) fn run(
    program: &Program,
    root: &Path,
    request: Request,
    deadline: Instant,
    canceled: &AtomicBool,
    before_dispatch: impl Fn() -> Result<(), String>,
    mut emit: impl FnMut(StreamDelta),
) -> Result<String, Error> {
    let output = execute(program, root, request, deadline, canceled, before_dispatch)?;
    let text = clean(&output);
    if text.trim().is_empty() {
        return Err(Error::Invalid("the CLI returned an empty answer"));
    }
    let answer = reply::sanitize_reply(&text);
    if answer.is_empty() {
        return Err(Error::Invalid("the CLI returned no final answer"));
    }
    static THINK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<think\b[^>]*>(.*?)</think\s*>").expect("static reasoning pattern")
    });
    let mut reasoning = Zeroizing::new(String::new());
    for block in THINK.captures_iter(&text) {
        if !reasoning.is_empty() {
            reasoning.push('\n');
        }
        reasoning.push_str(block[1].trim());
    }
    if !reasoning.is_empty() {
        emit(StreamDelta::Reasoning(reasoning.to_string()));
    }
    emit(StreamDelta::Text(answer.to_string()));
    Ok(answer.to_string())
}

fn execute(
    program: &Program,
    root: &Path,
    request: Request,
    deadline: Instant,
    canceled: &AtomicBool,
    before_dispatch: impl Fn() -> Result<(), String>,
) -> Result<Zeroizing<String>, Error> {
    let deadline = deadline.min(Instant::now() + MAX_DURATION);
    let mut output = execute_capture(
        program,
        root,
        request,
        deadline,
        canceled,
        Capture::Answer,
        before_dispatch,
    )?;
    let text = String::from_utf8(std::mem::take(&mut *output.stdout))
        .map(Zeroizing::new)
        .map_err(|error| {
            let _bytes = Zeroizing::new(error.into_bytes());
            Error::Invalid("CLI stdout is not UTF-8")
        })?;
    if output.credential.as_deref().is_some_and(|key| {
        super::credential_echo(&text, key) || super::credential_echo(&clean(&text), key)
    }) {
        return Err(Error::Review(
            "The MiniMax CLI response echoed a credential.".into(),
        ));
    }
    Ok(text)
}

fn execute_capture(
    program: &Program,
    root: &Path,
    request: Request,
    deadline: Instant,
    canceled: &AtomicBool,
    capture: Capture,
    before_dispatch: impl Fn() -> Result<(), String>,
) -> Result<ProcessOutput, Error> {
    active(deadline, canceled)?;
    if !request.matches(program, root) {
        return Err(Error::Review(
            "The reviewed CLI command or destination changed.".into(),
        ));
    }
    let changed = || {
        Error::Review(
            "The CLI executable changed or is unavailable. Review it again in AI Manager.".into(),
        )
    };
    let mut opened = executable::open(&program.path).map_err(|_| changed())?;
    executable::require_native_image(&mut opened.file).map_err(|_| changed())?;
    let digest = executable::digest(&mut opened.file, Some(canceled));
    active(deadline, canceled)?;
    if digest.map_err(|_| changed())? != program.fingerprint {
        return Err(changed());
    }
    let scratch = Scratch::named(root, &request.name)
        .map_err(|_| Error::Review("Could not prepare a private CLI working directory.".into()))?;
    #[cfg(target_os = "macos")]
    let image = macos::Image::prepare(program, &mut opened.file, &scratch, deadline, canceled)?;
    #[cfg(target_os = "macos")]
    let launch_image = &image;
    #[cfg(not(target_os = "macos"))]
    let launch_image = &opened.file;
    let files = request.uses_files();
    let mut payload = request.payload;
    let mut stdin = payload.take_stdin();
    let credential = if let Some(minimax) = &program.minimax {
        before_dispatch().map_err(Error::Review)?;
        active(deadline, canceled)?;
        let invocation = minimax.prepare(&stdin, deadline, canceled)?;
        stdin = invocation.wire;
        Some(invocation.key)
    } else {
        None
    };
    let authorize = || {
        before_dispatch().map_err(Error::Review)?;
        scratch
            .check()
            .map_err(|_| Error::Review("The private CLI working directory changed.".into()))?;
        if files {
            payload.publish(&scratch, deadline, canceled)?;
            before_dispatch().map_err(Error::Review)?;
            scratch
                .check()
                .map_err(|_| Error::Review("The private CLI working directory changed.".into()))?;
            payload.verify(&scratch)?;
        }
        opened.recheck(&program.path).map_err(|_| changed())?;
        #[cfg(target_os = "macos")]
        image.require_current(&opened.file).map_err(|_| changed())?;
        if let (Some(minimax), Some(key)) = (&program.minimax, &credential) {
            minimax.require_current_key(key)?;
        }
        active(deadline, canceled)
    };
    let mut output = process::run(
        &request.launch,
        launch_image,
        &scratch.path,
        ProcessInput {
            stdin,
            deadline,
            canceled,
            capture,
        },
        authorize,
    )?;
    active(deadline, canceled)?;
    output.credential = credential;
    Ok(output)
}

fn clean(output: &str) -> Zeroizing<String> {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[PX^_][^\x1b]*\x1b\\)",
        )
        .expect("static terminal control pattern")
    });
    Zeroizing::new(
        ANSI.replace_all(output, "")
            .chars()
            .filter(|ch| {
                matches!(ch, '\n' | '\t' | '\r')
                    || (!ch.is_control() && crate::session_journal::is_display_character(*ch))
            })
            .collect(),
    )
}

fn active(deadline: Instant, canceled: &AtomicBool) -> Result<(), Error> {
    if canceled.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::TimedOut)
    } else {
        Ok(())
    }
}

struct Scratch {
    path: PathBuf,
    directory: Option<Directory>,
    #[cfg(windows)]
    ancestors: Vec<std::fs::File>,
}

impl Scratch {
    fn named(root: &Path, name: &str) -> io::Result<Self> {
        #[cfg(unix)]
        let parent = Directory::create_owner_controlled(root)?;
        #[cfg(windows)]
        let parent = Directory::open(root, true)?;
        let directory = parent.create_private_subdirectory(OsStr::new(name))?;
        let scratch = Self {
            path: root.join(name),
            directory: Some(directory),
            #[cfg(windows)]
            ancestors: Vec::new(),
        };
        #[cfg(windows)]
        let scratch = {
            let mut scratch = scratch;
            scratch.ancestors = executable::lock_windows_ancestors(&scratch.path.join(".launch"))?;
            scratch
        };
        scratch.check()?;
        Ok(scratch)
    }

    fn check(&self) -> io::Result<()> {
        #[cfg(unix)]
        let current = Directory::open_owner_controlled(&self.path)?;
        #[cfg(windows)]
        let current = Directory::open(&self.path, false)?;
        if current.same_as(self.directory.as_ref().expect("owned CLI work directory"))? {
            Ok(())
        } else {
            Err(io::Error::other("The CLI working directory changed"))
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.check().is_ok() {
            self.directory = None;
            // Release the leaf for deletion, retaining the ancestor locks through cleanup.
            #[cfg(windows)]
            drop(self.ancestors.pop());
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
#[path = "cli/unix.rs"]
mod process;
#[cfg(windows)]
#[path = "cli/windows.rs"]
mod process;

#[cfg(test)]
pub(crate) fn fixture_executable(root: &Path) -> PathBuf {
    let source = root.join("fixture.rs");
    std::fs::write(&source, include_str!("cli/fixture.rs")).unwrap();
    let fixture = root.join(format!("cli-fixture{}", std::env::consts::EXE_SUFFIX));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(rustc)
        .args([
            "--edition=2024",
            "--crate-name",
            "kortty_cli_fixture",
            "--emit=link",
            "-C",
            "debuginfo=0",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&fixture)
        .current_dir(root)
        .output()
        .expect("start the installed Rust compiler for the native CLI fixture");
    assert!(
        output.status.success(),
        "native CLI fixture compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture
}

#[cfg(test)]
pub(crate) fn fixture_arguments(test: &str) -> Vec<String> {
    vec![test.into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_preset_binds_the_reviewed_endpoints_and_uses_stdin() {
        #[cfg(unix)]
        let executable = PathBuf::from("/fixture/codex");
        #[cfg(windows)]
        let executable = PathBuf::from(r"C:\fixture\codex.exe");
        let mut arguments: Vec<_> = CODEX_STDIN
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let program = Program::approved(executable.clone(), &arguments, "", [1; 32]).unwrap();
        let request = Request::prepare(
            &program,
            executable.parent().unwrap(),
            "System",
            &[],
            "User",
        )
        .unwrap();
        assert!(
            program
                .destination()
                .contains("api.openai.com or chatgpt.com")
        );
        assert!(
            request
                .command_review()
                .contains("features.shell_tool=false")
        );
        assert_eq!(request.primary().0, "Exact UTF-8 stdin");
        assert!(
            !request
                .launch
                .arguments
                .iter()
                .any(|argument| argument.contains("User"))
        );

        arguments[1] = "--sandbox=workspace-write".into();
        let changed = Program::approved(executable, &arguments, "", [1; 32]).unwrap();
        assert!(
            !changed
                .destination()
                .contains("api.openai.com or chatgpt.com")
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_cli_scratch_rejects_writable_unix_ancestors_before_creating_a_stage() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let shared = root.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o770)).unwrap();
        let profile = shared.join("profile");
        assert!(
            matches!(Scratch::named(&profile, ".ai-cli-guard"), Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(!profile.exists());

        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scratch = Scratch::named(&profile, ".ai-cli-guard").unwrap();
        scratch.check().unwrap();
        drop(scratch);
        assert!(profile.is_dir());
        assert!(!profile.join(".ai-cli-guard").exists());
    }

    #[cfg(windows)]
    #[test]
    fn configured_cli_scratch_locks_windows_ancestors_and_releases_them_after_cleanup() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let profile = root.join("profile");
        let scratch = Scratch::named(&profile, ".ai-cli-guard").unwrap();
        let stage = scratch.path.clone();
        let moved_stage = profile.join("moved-stage");
        let moved_profile = root.join("moved-profile");
        assert!(std::fs::rename(&stage, &moved_stage).is_err());
        assert!(std::fs::rename(&profile, &moved_profile).is_err());
        scratch.check().unwrap();
        drop(scratch);
        assert!(!stage.exists());
        std::fs::rename(profile, moved_profile).unwrap();
    }

    #[test]
    fn configured_cli_rejects_ambiguous_placeholders_and_expands_the_lm_studio_preset() {
        for invalid in [
            vec!["{systemPromptFile}"],
            vec!["{userPromptFile}"],
            vec!["{promptFile}", "{promptFile}"],
            vec!["{promptFile}", "{stdinPrompt}"],
            vec!["{systemPromptFile}", "{userPromptFile}", "{promptFile}"],
            vec!["--input={promptFile}"],
            vec!["{prompt}"],
        ] {
            assert!(!valid_arguments(
                &invalid.into_iter().map(String::from).collect::<Vec<_>>()
            ));
        }
        #[cfg(unix)]
        let executable = PathBuf::from("/fixture/lms");
        #[cfg(windows)]
        let executable = PathBuf::from(r"C:\fixture\lms.exe");
        let args = LM_STUDIO_STDIN
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        assert!(
            Program::approved(executable.clone(), &args, "--host=example.invalid", [1; 32])
                .is_err()
        );
        let program = Program::approved(executable.clone(), &args, "local/model", [1; 32]).unwrap();
        let request = Request::prepare(
            &program,
            executable.parent().unwrap(),
            "System",
            &[],
            "User",
        )
        .unwrap();
        assert_eq!(
            request.launch.arguments,
            ["chat", "local/model", "-p", LM_STUDIO_PROMPT]
        );
        assert_eq!(
            request.primary(),
            (
                "Exact UTF-8 stdin",
                "System prompt:\nSystem\n\nUser prompt:\nUser\n"
            )
        );
        assert!(request.secondary().is_none());
    }

    #[test]
    fn configured_cli_combined_file_is_exact_and_modified_or_cancelled_stages_are_removed() {
        use crate::local_ai::{Profile, start, test_support::exclusive_run};
        use std::cell::Cell;

        let _exclusive = exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let prepare = |fixture| {
            Program::approved(
                executable.clone(),
                &[fixture, "{promptFile}".into()],
                "",
                fingerprint,
            )
            .unwrap()
        };
        let program = prepare("fixture_files".into());
        let input = || {
            Request::prepare(
                &program,
                &root,
                "System å",
                &[("first", "second")],
                "User 漢",
            )
            .unwrap()
        };
        let request = input();
        let staged = root.join(&request.name);
        let expected = request.primary().1.to_owned();
        assert!(!staged.exists());
        let calls = Cell::new(0);
        let answer = run(
            &program,
            &root,
            request,
            Instant::now() + Duration::from_secs(15),
            &AtomicBool::new(false),
            || {
                let call = calls.replace(calls.get() + 1);
                assert_eq!(staged.join("prompt.txt").exists(), call == 1);
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert!(answer.contains("fixture final answer"));
        assert_eq!(
            std::fs::read_to_string(root.join("fixture-prompt.txt")).unwrap(),
            expected
        );
        assert!(!staged.exists());
        std::fs::remove_file(root.join("fixture-prompt.txt")).unwrap();

        let changed = input();
        let changed_stage = root.join(&changed.name);
        let rejected = run(
            &program,
            &root,
            changed,
            Instant::now() + Duration::from_secs(15),
            &AtomicBool::new(false),
            || {
                let path = changed_stage.join("prompt.txt");
                if path.exists() {
                    std::fs::write(path, b"altered outside the approved review").unwrap();
                }
                Ok(())
            },
            |_| {},
        )
        .unwrap_err();
        assert!(rejected.message().contains("prompt file changed"));
        assert!(!changed_stage.exists());
        assert!(!root.join("fixture-prompt.txt").exists());

        let waiting = prepare("fixture_files_waiting".into());
        let request = Request::prepare(&waiting, &root, "System", &[], "User").unwrap();
        let pending_stage = root.join(&request.name);
        let profile =
            Profile::configured_cli(waiting, root.clone(), "", Some(Duration::from_secs(30)));
        let pending = start(profile, |client| {
            client
                .reviewed_cli_chat(request, || Ok(()), |_| {})
                .map_err(Error::message)
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("fixture-started").exists() {
            assert!(Instant::now() < deadline, "CLI file fixture did not start");
            std::thread::sleep(POLL);
        }
        assert!(pending_stage.join("prompt.txt").is_file());
        #[cfg(windows)]
        assert!(std::fs::rename(&pending_stage, root.join("moved-active-stage")).is_err());
        let crate::local_ai::Pending { cancel, reply } = pending;
        drop(cancel);
        assert!(
            reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(!pending_stage.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn configured_cli_executes_the_held_image_after_a_linux_path_replacement() {
        use std::os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (path, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let program = Program::approved(
            path.clone(),
            &fixture_arguments("fixture_answer"),
            "",
            fingerprint,
        )
        .unwrap();
        let mut opened = executable::open(&path).unwrap();
        assert_eq!(
            executable::digest(&mut opened.file, None).unwrap(),
            fingerprint
        );
        let request =
            Request::prepare(&program, &root, "Fixture system", &[], "Fixture input").unwrap();
        let scratch = Scratch::named(&root, &request.name).unwrap();
        let expected = "Exact input for the verified inode";
        let bytes = process::run(
            &request.launch,
            &opened.file,
            &scratch.path,
            ProcessInput {
                stdin: Zeroizing::new(expected.into()),
                deadline: Instant::now() + Duration::from_secs(5),
                canceled: &AtomicBool::new(false),
                capture: Capture::Answer,
            },
            || {
                opened.recheck(&path).unwrap();
                std::fs::rename(&path, root.join("retired-cli")).unwrap();
                std::fs::write(&path, b"\x7fELF invalid replacement image").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                Ok(())
            },
        )
        .unwrap();
        assert!(
            std::str::from_utf8(&bytes.stdout)
                .unwrap()
                .contains("fixture final answer")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("fixture-stdin")).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read(root.join("fixture-argv0")).unwrap(),
            path.as_os_str().as_bytes()
        );
    }

    #[test]
    fn configured_cli_rechecks_proof_and_cleans_up_cancelled_or_oversized_runs() {
        use crate::local_ai::{Profile, start, test_support::exclusive_run};

        let _exclusive = exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let run_fixture = |name, fingerprint, allow| {
            let program = Program::approved(
                executable.clone(),
                &fixture_arguments(name),
                "",
                fingerprint,
            )
            .unwrap();
            let profile =
                Profile::configured_cli(program, root.clone(), "", Some(Duration::from_secs(30)));
            let request = profile
                .prepare_cli_chat("Fixture system", &[], "Fixture input")
                .unwrap();
            start(profile, move |client| {
                client
                    .reviewed_cli_chat(
                        request,
                        || {
                            if allow {
                                Ok(())
                            } else {
                                Err("The reviewed request changed.".into())
                            }
                        },
                        |_| {},
                    )
                    .map_err(Error::message)
            })
            .unwrap()
        };
        let mismatch = run_fixture("fixture_answer", [0; 32], true);
        assert!(
            mismatch
                .reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("executable changed")
        );
        drop(mismatch);
        let refused = run_fixture("fixture_answer", fingerprint, false);
        assert_eq!(
            refused.reply.recv_blocking().unwrap().unwrap_err(),
            "The reviewed request changed."
        );
        drop(refused);
        assert!(!root.join("fixture-stdin").exists());

        let oversize = run_fixture("fixture_too_large", fingerprint, true);
        assert!(
            oversize
                .reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("exceeds 64 KiB")
        );
        drop(oversize);

        let pending = run_fixture("fixture_waiting", fingerprint, true);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("fixture-started").exists() {
            assert!(
                Instant::now() < deadline,
                "CLI fixture never entered user code"
            );
            std::thread::sleep(POLL);
        }
        let crate::local_ai::Pending { cancel, reply } = pending;
        drop(cancel);
        assert!(
            reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ai-cli-")
        }));
    }

    #[test]
    fn configured_cli_workflow_checks_the_deadline_and_dispatch_guard() {
        use crate::local_ai::{Profile, start, test_support::exclusive_run};
        use std::sync::atomic::AtomicUsize;

        let _exclusive = exclusive_run();
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let program = Program::approved(
            executable,
            &fixture_arguments("fixture_answer"),
            "",
            fingerprint,
        )
        .unwrap();
        let profile = Profile::configured_cli(program, root.clone(), "", None);
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let start_request = |deadline, allow| {
            let attempts = attempts.clone();
            start(profile.clone(), move |client| {
                client
                    .complete_cli_before_guarded(
                        "Workflow system",
                        "Workflow input",
                        deadline,
                        || {
                            attempts.fetch_add(1, Ordering::Relaxed);
                            if allow {
                                Ok(())
                            } else {
                                Err("The workflow approval changed.".into())
                            }
                        },
                    )
                    .map_err(Error::message)
            })
            .unwrap()
        };

        let expired = start_request(Instant::now(), true);
        assert!(
            expired
                .reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("timed out")
        );
        drop(expired);
        assert_eq!(attempts.load(Ordering::Relaxed), 0);
        assert!(!root.join("fixture-stdin").exists());

        let denied = start_request(Instant::now() + Duration::from_secs(15), false);
        assert_eq!(
            denied.reply.recv_blocking().unwrap().unwrap_err(),
            "The workflow approval changed."
        );
        drop(denied);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(!root.join("fixture-stdin").exists());

        let approved = start_request(Instant::now() + Duration::from_secs(15), true);
        assert!(
            approved
                .reply
                .recv_blocking()
                .unwrap()
                .unwrap()
                .contains("fixture final answer")
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        assert_eq!(
            std::fs::read_to_string(root.join("fixture-stdin")).unwrap(),
            "System prompt:\nWorkflow system\n\nUser prompt:\nWorkflow input\n"
        );
    }
}
