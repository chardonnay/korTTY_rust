//! Reviewed local source tools shared by the file and snippet editors.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task};

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::cli::{self, Program};
use crate::session_security::IsolationLevel;
use crate::sftp_window::RemoteEditor;

const MAX_PREVIEW: usize = 8192;

mod builtin;
mod scripts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Format,
    CheckSyntax,
}

impl Action {
    const ALL: [Self; 2] = [Self::Format, Self::CheckSyntax];

    fn label(self) -> &'static str {
        match self {
            Self::Format => "Format Code…",
            Self::CheckSyntax => "Check Syntax…",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    Other,
    Lua,
    Shell,
    Rust,
    Go,
    Python,
    Ruby,
    Json,
    Xml,
    Yaml,
    Toml,
    Ini,
    Html,
    Css,
    JavaScript,
    TypeScript,
    Java,
    Groovy,
    Perl,
    Sql,
    Terraform,
}

impl Language {
    pub(crate) fn named(language: &str) -> Option<Self> {
        Some(match language.trim().to_ascii_lowercase().as_str() {
            "lua" | "luajit" => Self::Lua,
            "sh" | "bash" | "shell" | "shellscript" | "zsh" => Self::Shell,
            "rs" | "rust" => Self::Rust,
            "go" | "golang" => Self::Go,
            "py" | "pyw" | "python" => Self::Python,
            "rb" | "ruby" => Self::Ruby,
            "json" => Self::Json,
            "xml" => Self::Xml,
            "yml" | "yaml" => Self::Yaml,
            "toml" => Self::Toml,
            "ini" | "properties" => Self::Ini,
            "htm" | "html" => Self::Html,
            "css" => Self::Css,
            "js" | "javascript" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "typescript" | "mts" | "cts" => Self::TypeScript,
            "java" => Self::Java,
            "groovy" | "gradle" | "gvy" | "gy" | "gsh" | "jenkinsfile" => Self::Groovy,
            "pl" | "perl" | "pm" | "t" => Self::Perl,
            "sql" => Self::Sql,
            "tf" | "tfvars" | "terraform" => Self::Terraform,
            _ => return None,
        })
    }

    pub(crate) fn extension(path: &Path) -> Option<Self> {
        if path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.eq_ignore_ascii_case("Jenkinsfile"))
        {
            return Some(Self::Groovy);
        }
        Self::named(path.extension()?.to_str()?)
    }

    fn tool(self, action: Action) -> Option<Tool> {
        use scripts::Interpreter;

        let launcher = match self {
            Self::Ruby => Some(Interpreter::Ruby),
            Self::Python => Some(Interpreter::Python),
            Self::Perl | Self::Sql => Some(Interpreter::Perl),
            _ => None,
        };
        let (name, arguments): (_, &'static [&'static str]) = match (self, action) {
            (Self::Lua, Action::Format) => ("stylua", &["--verify", "-"]),
            (Self::Lua, Action::CheckSyntax) => ("luac", &["-p", "-"]),
            (Self::Shell, Action::Format) => ("shfmt", &[]),
            (Self::Rust, Action::Format) => (
                "rustfmt",
                &["--emit", "stdout", "--config", "skip_children=true"],
            ),
            (Self::Go, Action::Format) => ("gofmt", &[]),
            (Self::Python, Action::Format) => ("black", &["-q", "-"]),
            (Self::Ruby, Action::Format) => (
                "rubocop",
                &[
                    "-a",
                    "--stderr",
                    "--fail-level",
                    "fatal",
                    "--stdin",
                    "buffer.rb",
                ],
            ),
            (Self::Json, Action::Format) => return Some(Tool::BuiltIn(builtin::Format::Json)),
            (Self::Json, Action::CheckSyntax) => return Some(Tool::JsonSyntax),
            (Self::Ini, Action::Format) => return Some(Tool::BuiltIn(builtin::Format::Ini)),
            (Self::Groovy, Action::Format) => return Some(Tool::BuiltIn(builtin::Format::Groovy)),
            (Self::Xml, Action::Format) => (
                "xmllint",
                &[
                    "--nonet",
                    "--nocatalogs",
                    "--format",
                    "--encode",
                    "UTF-8",
                    "-",
                ],
            ),
            (Self::Yaml, Action::Format) => ("yamlfmt", &["-no_global_conf", "-in"]),
            (Self::Toml, Action::Format) => ("taplo", &["fmt", "--no-auto-config", "-"]),
            (Self::Html, Action::Format) => (
                "biome",
                &[
                    "format",
                    "--vcs-enabled=false",
                    "--use-editorconfig=false",
                    "--html-formatter-enabled=true",
                    "--html-formatter-whitespace-sensitivity=strict",
                    "--stdin-file-path=buffer.html",
                ],
            ),
            (Self::Css, Action::Format) => (
                "biome",
                &[
                    "format",
                    "--vcs-enabled=false",
                    "--use-editorconfig=false",
                    "--stdin-file-path=buffer.css",
                ],
            ),
            (Self::JavaScript, Action::Format) => (
                "biome",
                &[
                    "format",
                    "--vcs-enabled=false",
                    "--use-editorconfig=false",
                    "--stdin-file-path=buffer.js",
                ],
            ),
            (Self::TypeScript, Action::Format) => (
                "biome",
                &[
                    "format",
                    "--vcs-enabled=false",
                    "--use-editorconfig=false",
                    "--stdin-file-path=buffer.ts",
                ],
            ),
            (Self::Java, Action::Format) => (
                "google-java-format",
                &[
                    "--skip-sorting-imports",
                    "--skip-removing-unused-imports",
                    "-",
                ],
            ),
            (Self::Perl, Action::Format) => ("perltidy", &["-npro", "-st", "-se"]),
            (Self::Sql, Action::Format) => ("pg_format", &["--no-rcfile", "-"]),
            (Self::Terraform, Action::Format) => ("terraform", &["fmt", "-no-color", "-"]),
            _ => return None,
        };
        Some(Tool::External(ExternalTool {
            action,
            name,
            arguments,
            launcher,
        }))
    }
}

#[derive(Clone, Copy)]
enum Tool {
    BuiltIn(builtin::Format),
    JsonSyntax,
    External(ExternalTool),
}

impl Tool {
    fn action(self) -> Action {
        match self {
            Self::BuiltIn(_) => Action::Format,
            Self::JsonSyntax => Action::CheckSyntax,
            Self::External(tool) => tool.action,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::BuiltIn(format) => format.name(),
            Self::JsonSyntax => "JSON validation",
            Self::External(tool) => tool.name,
        }
    }
}

#[derive(Clone, Copy)]
struct ExternalTool {
    action: Action,
    name: &'static str,
    arguments: &'static [&'static str],
    launcher: Option<scripts::Interpreter>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Context {
    pub(crate) revision: u64,
    pub(crate) language: Language,
    pub(crate) limit: usize,
    pub(crate) remote: bool,
    pub(crate) writable: bool,
}

impl Context {
    fn supports(self, action: Action) -> bool {
        self.writable || action == Action::CheckSyntax
    }
}

struct Source {
    context: Context,
    tool: Tool,
    text: String,
    remote: Option<RemoteEditor>,
    schema: Option<Arc<crate::snippets::json_schema::Schema>>,
}

impl Source {
    fn allowed(&self) -> Result<(), String> {
        allowed(self.tool, self.remote.as_ref())
    }
}

#[derive(Clone)]
pub enum Message {
    Open(Action),
    Inspected(u64, Result<Arc<Plan>, String>),
    Approve(u64),
    Completed(u64, Result<Report, String>),
    Apply(u64),
    RevealDiagnostic(u64, usize),
    UseAi(u64),
    Cancel,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Editor tool action")
    }
}

impl Message {
    pub(crate) fn is_completion(&self) -> bool {
        matches!(self, Self::Inspected(..) | Self::Completed(..))
    }
}

pub struct Plan {
    operation: Operation,
    review: String,
}

enum Operation {
    BuiltIn(builtin::Format),
    JsonSyntax,
    External(Box<ExternalPlan>),
}

struct ExternalPlan {
    program: Program,
    script: Option<scripts::Launcher>,
}

#[derive(Clone)]
pub struct Report {
    notice: String,
    diagnostics: String,
    formatted: Option<Arc<str>>,
    marks: Vec<crate::snippets::json_schema::Mark>,
}

struct Stop(Arc<AtomicBool>);

impl Default for Stop {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

enum Phase {
    Inspecting(Stop),
    Review(Arc<Plan>),
    Running(Stop),
    Outcome(Report),
}

struct Request {
    token: u64,
    source: Arc<Source>,
    phase: Phase,
}

#[derive(Default)]
pub(crate) struct Panel {
    request: Option<Request>,
    fallback: Option<Fallback>,
    next_token: u64,
}

struct Fallback {
    token: u64,
    context: Context,
    action: Action,
    reason: String,
}

pub(crate) enum Update {
    None,
    Job(Job),
    Notice(String),
    Apply(Arc<str>),
    RevealDiagnostic(std::ops::Range<usize>),
    UseAi(Action),
}

pub(crate) struct Job {
    token: u64,
    source: Arc<Source>,
    cancel: Arc<AtomicBool>,
    plan: Option<Arc<Plan>>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
            && Arc::ptr_eq(&self.source, &other.source)
            && self.plan.is_some() == other.plan.is_some()
    }
}

impl Eq for Job {}

impl Job {
    pub(crate) fn task(self) -> Task<Message> {
        let (sender, receiver) = async_channel::bounded(1);
        let token = self.token;
        let running = self.plan.is_some();
        let thread = std::thread::Builder::new()
            .name("kortty-editor-tool".into())
            .spawn(move || {
                let _ = sender.send_blocking(self.run());
            });
        if thread.is_err() {
            return Task::done(job_ended(token, running));
        }
        Task::perform(
            async move {
                receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| job_ended(token, running))
            },
            |message| message,
        )
    }

    fn run(self) -> Message {
        if let Some(plan) = self.plan {
            Message::Completed(self.token, run_tool(&self.source, &plan, &self.cancel))
        } else {
            Message::Inspected(
                self.token,
                inspect(self.source.tool, &self.cancel).map(Arc::new),
            )
        }
    }
}

fn job_ended(token: u64, running: bool) -> Message {
    let error = Err("The editor tool worker ended without a result.".into());
    if running {
        Message::Completed(token, error)
    } else {
        Message::Inspected(token, Err("Could not inspect the editor tool.".into()))
    }
}

impl Panel {
    pub(crate) fn marks(&self) -> &[crate::snippets::json_schema::Mark] {
        match self.request.as_ref().map(|request| &request.phase) {
            Some(Phase::Outcome(report)) => &report.marks,
            _ => &[],
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.request.is_some() || self.fallback.is_some()
    }

    pub(crate) fn invalidate(&mut self, current: Option<Context>) {
        if self
            .request
            .as_ref()
            .is_some_and(|request| Some(request.source.context) != current)
        {
            self.request = None;
        }
        if self
            .fallback
            .as_ref()
            .is_some_and(|fallback| Some(fallback.context) != current)
        {
            self.fallback = None;
        }
    }

    pub(crate) fn update(
        &mut self,
        message: Message,
        current: Option<Context>,
        remote: Option<&RemoteEditor>,
        read: impl FnOnce() -> String,
    ) -> Update {
        self.update_with_schema(message, current, remote, None, read)
    }

    pub(crate) fn update_with_schema(
        &mut self,
        message: Message,
        current: Option<Context>,
        remote: Option<&RemoteEditor>,
        schema: Option<&Arc<crate::snippets::json_schema::Schema>>,
        read: impl FnOnce() -> String,
    ) -> Update {
        self.invalidate(current);
        match message {
            Message::Cancel => {
                self.request = None;
                self.fallback = None;
            }
            Message::UseAi(token)
                if self
                    .fallback
                    .as_ref()
                    .is_some_and(|fallback| fallback.token == token) =>
            {
                let fallback = self.fallback.take().expect("matching fallback");
                return Update::UseAi(fallback.action);
            }
            Message::Open(action) if !self.active() => {
                let Some(context) = current.filter(|context| context.supports(action)) else {
                    return Update::None;
                };
                let text = read();
                if text.len() > context.limit || text.contains('\0') || text.trim().is_empty() {
                    return Update::Notice(
                        "Enter nonempty UTF-8 source within the editor size limit.".into(),
                    );
                }
                self.next_token = self.next_token.wrapping_add(1);
                let token = self.next_token;
                let Some(tool) = context.language.tool(action) else {
                    self.fallback = Some(Fallback {
                        token,
                        context,
                        action,
                        reason: "No local tool supports this action for the editor language."
                            .into(),
                    });
                    return Update::None;
                };
                if let Err(reason) = allowed(tool, remote) {
                    return Update::Notice(reason);
                }
                let source = Arc::new(Source {
                    context,
                    tool,
                    text,
                    remote: remote.cloned(),
                    schema: schema.filter(|_| matches!(tool, Tool::JsonSyntax)).cloned(),
                });
                let stop = Stop::default();
                let cancel = Arc::clone(&stop.0);
                self.request = Some(Request {
                    token,
                    source: Arc::clone(&source),
                    phase: Phase::Inspecting(stop),
                });
                return Update::Job(Job {
                    token,
                    source,
                    cancel,
                    plan: None,
                });
            }
            message => {
                let Some(request) = self.request.as_mut() else {
                    return Update::None;
                };
                let token = request.token;
                match message {
                    Message::Inspected(id, result)
                        if id == token && matches!(request.phase, Phase::Inspecting(_)) =>
                    {
                        match result {
                            Ok(plan) => request.phase = Phase::Review(plan),
                            Err(reason) => {
                                self.fallback = Some(Fallback {
                                    token,
                                    context: request.source.context,
                                    action: request.source.tool.action(),
                                    reason,
                                });
                                self.request = None;
                            }
                        }
                    }
                    Message::Approve(id)
                        if id == token
                            && let Phase::Review(plan) = &request.phase =>
                    {
                        if let Err(reason) = request.source.allowed() {
                            self.request = None;
                            return Update::Notice(reason);
                        }
                        let stop = Stop::default();
                        let job = Job {
                            token,
                            source: Arc::clone(&request.source),
                            cancel: Arc::clone(&stop.0),
                            plan: Some(Arc::clone(plan)),
                        };
                        request.phase = Phase::Running(stop);
                        return Update::Job(job);
                    }
                    Message::Completed(id, result)
                        if id == token && matches!(request.phase, Phase::Running(_)) =>
                    {
                        match result {
                            Ok(report) => request.phase = Phase::Outcome(report),
                            Err(reason) => {
                                self.request = None;
                                return Update::Notice(reason);
                            }
                        }
                    }
                    Message::Apply(id)
                        if id == token
                            && let Phase::Outcome(report) = &request.phase
                            && let Some(output) = report.formatted.clone() =>
                    {
                        let access = request.source.allowed();
                        self.request = None;
                        return match access {
                            Ok(()) => Update::Apply(output),
                            Err(reason) => Update::Notice(reason),
                        };
                    }
                    Message::RevealDiagnostic(id, index)
                        if id == token
                            && let Phase::Outcome(report) = &request.phase
                            && let Some(mark) = report.marks.get(index) =>
                    {
                        return Update::RevealDiagnostic(mark.bytes.clone());
                    }
                    _ => {}
                }
            }
        }
        Update::None
    }

    pub(crate) fn invalidate_schema(
        &mut self,
        schema: Option<&Arc<crate::snippets::json_schema::Schema>>,
    ) {
        if self.request.as_ref().is_some_and(|request| {
            matches!(request.source.tool, Tool::JsonSyntax)
                && request.source.schema.as_ref() != schema
        }) {
            self.request = None;
        }
    }

    pub(crate) fn actions(
        &self,
        current: Option<Context>,
    ) -> impl Iterator<Item = iced::widget::Button<'_, Message>> {
        Action::ALL.into_iter().filter_map(move |action| {
            current.filter(|context| context.supports(action)).map(|_| {
                button(action.label())
                    .on_press_maybe((!self.active()).then_some(Message::Open(action)))
            })
        })
    }

    pub(crate) fn view(&self) -> Option<Element<'_, Message>> {
        if let Some(fallback) = &self.fallback {
            return Some(
                column![
                    text(&fallback.reason),
                    row![
                        button("Continue with AI…").on_press(Message::UseAi(fallback.token)),
                        button("Cancel").on_press(Message::Cancel),
                    ]
                    .spacing(8)
                ]
                .spacing(8)
                .into(),
            );
        }
        let request = self.request.as_ref()?;
        let cancel = button("Cancel").on_press(Message::Cancel);
        let content: Element<'_, Message> = match &request.phase {
            Phase::Inspecting(stop) | Phase::Running(stop) => {
                let _ = stop;
                let label = if matches!(request.phase, Phase::Inspecting(_)) { "Preparing the editor tool review…" } else { "Running the approved editor tool…" };
                row![text(label), cancel].spacing(8).into()
            }
            Phase::Review(plan) => column![
                text(format!("Run {} on the current editor buffer?", request.source.tool.name())),
                text(&plan.review).size(12),
                text(match (&plan.operation, request.source.context.remote) {
                    (Operation::BuiltIn(_), _) => "The buffer is formatted in korTTY. Review the proposed result before applying it.",
                    (Operation::JsonSyntax, _) if request.source.schema.is_some() => "Check JSON syntax and the saved or loaded schema in korTTY.",
                    (Operation::JsonSyntax, _) => "Check JSON syntax in korTTY.",
                    (Operation::External(_), true) => "The exact remote file buffer will be passed to this local program on standard input.",
                    (Operation::External(_), false) => "The exact editor buffer will be passed to this local program on standard input.",
                }).size(13),
                row![button("Approve and run").on_press(Message::Approve(request.token)), cancel].spacing(8),
            ].spacing(4).into(),
            Phase::Outcome(report) => {
                let mut body = column![text(&report.notice)];
                if !report.diagnostics.is_empty() {
                    body = body.push(scrollable(text(&report.diagnostics).size(13)).height(90));
                }
                if !report.marks.is_empty() {
                    let locations = row(report.marks.iter().enumerate().map(|(index, mark)| {
                        let label: String = mark.label.chars().take(60).collect();
                        button(text(label).size(12))
                            .on_press(Message::RevealDiagnostic(request.token, index)).into()
                    })).spacing(5).wrap();
                    body = body.push(locations);
                }
                let mut choices = row![];
                if let Some(formatted) = &report.formatted {
                    let preview = &formatted[..formatted.floor_char_boundary(MAX_PREVIEW.min(formatted.len()))];
                    if preview.len() < formatted.len() {
                        body = body.push(text("Showing the first 8 KiB of the proposed formatting.").size(13));
                    }
                    body = body.push(scrollable(text(preview).size(13)).height(120));
                    choices = choices.push(button("Apply formatting").on_press(Message::Apply(request.token)));
                }
                body.push(choices.push(button("Close").on_press(Message::Cancel)).spacing(8)).spacing(5).into()
            }
        };
        Some(iced::widget::container(content).width(Length::Fill).into())
    }
}

fn allowed(tool: Tool, remote: Option<&RemoteEditor>) -> Result<(), String> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some()
        || matches!(tool, Tool::External(_)) && policy.minimum_isolation != IsolationLevel::None
        || remote.is_some_and(|editor| {
            !editor.allows_source_tool(&policy, tool.action() == Action::Format)
        })
    {
        return Err("Managed policy blocks running this tool on the editor buffer.".into());
    }
    Ok(())
}

fn inspect(tool: Tool, cancel: &AtomicBool) -> Result<Plan, String> {
    if matches!(tool, Tool::JsonSyntax | Tool::BuiltIn(_)) {
        return inspect_paths(tool, &[], cancel);
    }
    let path = std::env::var_os("PATH");
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };
    let search = search_paths(
        path.as_deref(),
        home.as_deref().map(Path::new),
        cfg!(target_os = "macos"),
    );
    inspect_paths(tool, &search, cancel)
}

fn inspect_paths(tool: Tool, search: &[PathBuf], cancel: &AtomicBool) -> Result<Plan, String> {
    let tool = match tool {
        Tool::BuiltIn(format) => {
            return Ok(Plan {
                operation: Operation::BuiltIn(format),
                review: format.name().into(),
            });
        }
        Tool::JsonSyntax => {
            return Ok(Plan {
                operation: Operation::JsonSyntax,
                review: "Built-in JSON validation".into(),
            });
        }
        Tool::External(tool) => tool,
    };
    let filename = format!("{}{}", tool.name, std::env::consts::EXE_SUFFIX);
    for directory in search {
        if cancel.load(Ordering::Acquire) {
            return Err("The editor tool review was cancelled.".into());
        }
        let candidate = directory.join(&filename);
        if let Ok(plan) = inspect_executable(tool, &candidate, cancel) {
            return Ok(plan);
        }
    }
    if let Some(interpreter) = tool.launcher {
        if let Some(plan) = scripts::inspect(tool, interpreter, search, cancel) {
            return Ok(plan);
        }
        return Err(format!(
            "Install {} and {} in trusted PATH directories, then try again.",
            interpreter.name(),
            tool.name
        ));
    }
    Err(format!(
        "Install the native {} executable in a trusted PATH directory and try again.",
        tool.name
    ))
}

fn inspect_executable(
    tool: ExternalTool,
    candidate: &Path,
    cancel: &AtomicBool,
) -> Result<Plan, String> {
    let (path, fingerprint) = Program::inspect_cancellable(candidate, Some(cancel))?;
    let arguments = tool
        .arguments
        .iter()
        .map(|argument| (*argument).into())
        .collect::<Vec<_>>();
    let program = Program::approved(path, &arguments, "", fingerprint)?;
    Ok(Plan {
        review: program.command_review(),
        operation: Operation::External(Box::new(ExternalPlan {
            program,
            script: None,
        })),
    })
}

fn search_paths(path: Option<&OsStr>, home: Option<&Path>, macos: bool) -> Vec<PathBuf> {
    let mut paths = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    if macos {
        paths.extend(
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/local/bin",
                "/usr/bin",
            ]
            .map(PathBuf::from),
        );
    }
    if let Some(home) = home.filter(|home| home.is_absolute()) {
        paths.extend([home.join(".cargo").join("bin"), home.join("go").join("bin")]);
        if !cfg!(windows) {
            paths.push(home.join(".local").join("bin"));
        }
    }
    let mut seen = std::collections::HashSet::<OsString>::new();
    paths.retain(|path| seen.insert(path.as_os_str().to_owned()));
    paths
}

fn run_tool(source: &Source, plan: &Plan, cancel: &AtomicBool) -> Result<Report, String> {
    if matches!(plan.operation, Operation::JsonSyntax) {
        source.allowed()?;
        let checked = crate::snippets::json_schema::check(
            &source.text,
            source.schema.as_deref(),
            cancel,
            MAX_PREVIEW,
        )?;
        source.allowed()?;
        return Ok(Report {
            notice: checked.notice.into(),
            diagnostics: checked.diagnostics,
            formatted: None,
            marks: checked.marks,
        });
    }
    if let Operation::BuiltIn(format) = &plan.operation {
        return run_builtin(source, *format, cancel);
    }
    let root = crate::profile::root()
        .and_then(|root| root.canonicalize())
        .map_err(|_| "The private editor tool directory is unavailable.")?;
    run_tool_at(source, plan, &root, cancel)
}

fn run_tool_at(
    source: &Source,
    plan: &Plan,
    root: &Path,
    cancel: &AtomicBool,
) -> Result<Report, String> {
    let Operation::External(plan) = &plan.operation else {
        return Err("The reviewed external editor tool is unavailable.".into());
    };
    let context = source.context;
    source.allowed()?;
    if context.language == Language::Xml && source.text.contains("<!DOCTYPE") {
        return Err("XML formatting requires a document without a DTD.".into());
    }
    let output_limit = if source.tool.action() == Action::Format {
        context.limit
    } else {
        MAX_PREVIEW
    };
    let output = cli::run_editor_tool(
        &plan.program,
        root,
        &source.text,
        plan.script.as_ref().map(scripts::Launcher::text),
        output_limit,
        cancel,
        || {
            source.allowed()?;
            if let Some(script) = &plan.script {
                script.require_current(cancel)?;
            }
            Ok(())
        },
    )?;
    source.allowed()?;
    if !output.success {
        return Ok(Report {
            notice: format!("{} reported an error.", source.tool.name()),
            diagnostics: output.diagnostics,
            formatted: None,
            marks: Vec::new(),
        });
    }
    format_report(source, output.stdout, output.diagnostics)
}

fn run_builtin(
    source: &Source,
    format: builtin::Format,
    cancel: &AtomicBool,
) -> Result<Report, String> {
    source.allowed()?;
    let output = format.run(&source.text, source.context.limit, cancel)?;
    source.allowed()?;
    format_report(source, output, String::new())
}

fn format_report(source: &Source, stdout: String, diagnostics: String) -> Result<Report, String> {
    let (notice, formatted) = match source.tool.action() {
        Action::CheckSyntax => ("Syntax check passed.", None),
        Action::Format if stdout.contains('\0') || stdout.trim().is_empty() => {
            return Err("The formatter returned empty or invalid source text.".into());
        }
        Action::Format if stdout == source.text => ("The source is already formatted.", None),
        Action::Format => (
            "Review the formatted buffer before applying it.",
            Some(Arc::from(stdout)),
        ),
    };
    Ok(Report {
        notice: notice.into(),
        diagnostics,
        formatted,
        marks: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn external(tool: Tool) -> ExternalTool {
        let Tool::External(tool) = tool else {
            panic!("Expected an external tool");
        };
        tool
    }

    #[test]
    fn editor_tool_discovery_ignores_relative_paths_and_knows_finder_locations() {
        let home = std::env::temp_dir();
        let path = std::env::join_paths([Path::new("."), &home, &home]).unwrap();
        let paths = search_paths(Some(&path), Some(&home), false);
        let mut expected = vec![
            home.clone(),
            home.join(".cargo").join("bin"),
            home.join("go").join("bin"),
        ];
        if !cfg!(windows) {
            expected.push(home.join(".local").join("bin"));
        }
        assert_eq!(paths, expected);
        assert!(search_paths(None, None, true).contains(&PathBuf::from("/opt/homebrew/bin")));
        assert_eq!(
            external(Language::Lua.tool(Action::CheckSyntax).unwrap()).arguments,
            ["-p", "-"]
        );
    }

    #[test]
    fn a_tool_reply_cannot_replace_a_changed_editor_revision() {
        let context = Context {
            revision: 1,
            language: Language::Lua,
            limit: 100,
            remote: false,
            writable: true,
        };
        let mut panel = Panel::default();
        let source = Arc::new(Source {
            context,
            tool: Language::Lua.tool(Action::Format).unwrap(),
            text: "return 1".into(),
            remote: None,
            schema: None,
        });
        let stop = Stop::default();
        let canceled = Arc::clone(&stop.0);
        panel.request = Some(Request {
            token: 1,
            source,
            phase: Phase::Running(stop),
        });
        let formatted = Report {
            notice: "fixture".into(),
            diagnostics: String::new(),
            formatted: Some("return 1\n".into()),
            marks: Vec::new(),
        };
        let changed = Context {
            revision: 2,
            ..context
        };
        assert!(matches!(
            panel.update(
                Message::Completed(1, Ok(formatted)),
                Some(changed),
                None,
                String::new
            ),
            Update::None
        ));
        assert!(canceled.load(Ordering::Acquire));
        assert!(!panel.active());
    }

    #[test]
    fn json_syntax_tool_reviews_the_schema_and_reports_paths_without_editing() {
        let schema = Arc::new(
            crate::snippets::json_schema::Schema::parse(
                br##"{
            "properties": {"limit": {"$ref": "#/$defs/count"}},
            "$defs": {"count": {"type": "integer"}}
        }"##,
            )
            .unwrap(),
        );
        let context = Context {
            revision: 1,
            language: Language::Json,
            limit: 4096,
            remote: false,
            writable: false,
        };
        let mut panel = Panel::default();
        let input = r#"{"limit":"sensitive input value"}"#;
        let Update::Job(inspect) = panel.update_with_schema(
            Message::Open(Action::CheckSyntax),
            Some(context),
            None,
            Some(&schema),
            || input.into(),
        ) else {
            panic!("expected built-in validation review");
        };
        panel.update(inspect.run(), Some(context), None, String::new);
        let Update::Job(approved) =
            panel.update(Message::Approve(1), Some(context), None, String::new)
        else {
            panic!("expected the approved validation job");
        };
        let Message::Completed(_, Ok(report)) = approved.run() else {
            panic!("expected a validation report");
        };
        assert_eq!(report.notice, "JSON does not match the schema.");
        assert!(report.diagnostics.contains("/limit"));
        assert!(!report.diagnostics.contains("sensitive input value"));
        assert!(report.formatted.is_none());
        panel.update(
            Message::Completed(1, Ok(report)),
            Some(context),
            None,
            String::new,
        );
        let Update::RevealDiagnostic(range) = panel.update(
            Message::RevealDiagnostic(1, 0),
            Some(context),
            None,
            String::new,
        ) else {
            panic!("expected the selected diagnostic location");
        };
        assert_eq!(&input[range], "\"sensitive input value\"");
        panel.invalidate_schema(None);
        assert!(!panel.active());
    }

    #[test]
    fn editor_tool_fallback_requires_current_explicit_selection() {
        let context = Context {
            revision: 1,
            language: Language::Other,
            limit: 100,
            remote: false,
            writable: true,
        };
        let mut panel = Panel::default();
        let open = |panel: &mut Panel, context| {
            panel.update(Message::Open(Action::Format), Some(context), None, || {
                "source".into()
            })
        };
        assert!(matches!(open(&mut panel, context), Update::None));
        assert!(panel.active());
        let changed = Context {
            revision: 2,
            ..context
        };
        assert!(matches!(
            panel.update(Message::UseAi(1), Some(changed), None, String::new),
            Update::None
        ));
        assert!(!panel.active());

        let native = Context {
            language: Language::Lua,
            ..changed
        };
        assert!(matches!(open(&mut panel, native), Update::Job(_)));
        assert!(matches!(
            panel.update(
                Message::Inspected(2, Err("Formatter unavailable.".into())),
                Some(native),
                None,
                String::new
            ),
            Update::None
        ));
        assert!(matches!(
            panel.update(Message::UseAi(2), Some(native), None, String::new),
            Update::UseAi(Action::Format)
        ));
        assert!(!panel.active());
    }

    fn run_fixture(
        language: Language,
        action: Action,
        code: &str,
        inspect: impl FnOnce(Tool, &AtomicBool) -> Plan,
    ) -> (Panel, Context, Report) {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let context = Context {
            revision: 1,
            language,
            limit: 1024,
            remote: false,
            writable: true,
        };
        let mut panel = Panel::default();
        let Update::Job(review) =
            panel.update(Message::Open(action), Some(context), None, || code.into())
        else {
            panic!("Could not start the editor tool review");
        };
        let plan = inspect(review.source.tool, &review.cancel);
        assert!(matches!(
            panel.update(
                Message::Inspected(review.token, Ok(Arc::new(plan))),
                Some(context),
                None,
                String::new
            ),
            Update::None
        ));
        assert!(matches!(
            panel.update(
                Message::Apply(review.token),
                Some(context),
                None,
                String::new
            ),
            Update::None
        ));
        let Update::Job(approved) = panel.update(
            Message::Approve(review.token),
            Some(context),
            None,
            String::new,
        ) else {
            panic!("The editor tool did not accept its verified executable");
        };
        let plan = approved.plan.as_deref().unwrap();
        let report = match &plan.operation {
            Operation::BuiltIn(format) => run_builtin(&approved.source, *format, &approved.cancel),
            Operation::JsonSyntax => run_tool(&approved.source, plan, &approved.cancel),
            Operation::External(_) => run_tool_at(&approved.source, plan, &root, &approved.cancel),
        }
        .unwrap();
        assert!(matches!(
            panel.update(
                Message::Completed(review.token, Ok(report.clone())),
                Some(context),
                None,
                String::new
            ),
            Update::None
        ));
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "tool scratch is removed"
        );
        (panel, context, report)
    }

    #[test]
    #[ignore = "requires an installed native Lua compiler at KORTTY_TEST_LUAC"]
    fn editor_tool_native_luac() {
        let path =
            PathBuf::from(std::env::var_os("KORTTY_TEST_LUAC").expect("absolute native luac path"));
        let inspect =
            |tool, cancel: &AtomicBool| inspect_executable(external(tool), &path, cancel).unwrap();
        let (_, _, valid) = run_fixture(
            Language::Lua,
            Action::CheckSyntax,
            "return 'café'\n",
            inspect,
        );
        assert_eq!(valid.notice, "Syntax check passed.");
        assert!(valid.formatted.is_none());
        let (_, _, invalid) = run_fixture(Language::Lua, Action::CheckSyntax, "local =\n", inspect);
        assert_eq!(invalid.notice, "luac reported an error.");
        assert!(
            invalid.diagnostics.contains(":1:"),
            "{}",
            invalid.diagnostics
        );
        assert!(invalid.formatted.is_none());
    }

    #[test]
    #[ignore = "requires an installed native StyLua formatter at KORTTY_TEST_STYLUA"]
    fn editor_tool_native_stylua() {
        let path = PathBuf::from(
            std::env::var_os("KORTTY_TEST_STYLUA").expect("absolute native StyLua path"),
        );
        let source = "local n=1\nreturn n\n";
        let inspect =
            |tool, cancel: &AtomicBool| inspect_executable(external(tool), &path, cancel).unwrap();
        let (mut panel, context, report) =
            run_fixture(Language::Lua, Action::Format, source, inspect);
        assert_eq!(report.formatted.as_deref(), Some("local n = 1\nreturn n\n"));
        let token = panel.request.as_ref().unwrap().token;
        assert!(
            matches!(panel.update(Message::Apply(token), Some(context), None, String::new), Update::Apply(text) if text.as_ref() == "local n = 1\nreturn n\n")
        );
        let (mut panel, context, _) = run_fixture(Language::Lua, Action::Format, source, inspect);
        let token = panel.request.as_ref().unwrap().token;
        let changed = Context {
            revision: context.revision + 1,
            ..context
        };
        assert!(matches!(
            panel.update(Message::Apply(token), Some(changed), None, String::new),
            Update::None
        ));
    }

    fn installed_formatter(
        language: Language,
        directory_variable: &str,
        source: &str,
    ) -> (Panel, Context, Report) {
        let directory = PathBuf::from(
            std::env::var_os(directory_variable).expect("absolute formatter directory"),
        );
        assert!(directory.is_absolute());
        let search = std::iter::once(directory)
            .chain(
                std::env::var_os("PATH")
                    .iter()
                    .flat_map(std::env::split_paths)
                    .filter(|path| path.is_absolute()),
            )
            .collect::<Vec<_>>();
        run_fixture(language, Action::Format, source, |tool, cancel| {
            inspect_paths(tool, &search, cancel).unwrap()
        })
    }

    #[test]
    #[ignore = "requires an installed Black formatter directory at KORTTY_TEST_BLACK_DIR"]
    fn editor_tool_native_black() {
        let (mut panel, context, report) =
            installed_formatter(Language::Python, "KORTTY_TEST_BLACK_DIR", "name='café'\n");
        assert_eq!(
            report.formatted.as_deref(),
            Some("name = \"café\"\n"),
            "{}",
            report.diagnostics
        );
        let token = panel.request.as_ref().unwrap().token;
        assert!(matches!(
            panel.update(Message::Apply(token), Some(context), None, String::new),
            Update::Apply(_)
        ));
    }

    #[test]
    #[ignore = "requires an installed RuboCop formatter directory at KORTTY_TEST_RUBOCOP_DIR"]
    fn editor_tool_native_rubocop() {
        let (_, _, report) = installed_formatter(
            Language::Ruby,
            "KORTTY_TEST_RUBOCOP_DIR",
            "puts( 'café' )\n",
        );
        let formatted = report
            .formatted
            .as_deref()
            .unwrap_or_else(|| panic!("{}: {}", report.notice, report.diagnostics));
        assert!(formatted.contains("café"));
        assert!(!formatted.contains("( '"));
        assert!(!formatted.contains("offense"));
    }

    #[test]
    fn built_in_editor_format_requires_review_and_apply() {
        let (mut panel, context, report) = run_fixture(
            Language::Json,
            Action::Format,
            "{\"port\":80}",
            |tool, cancel| inspect_paths(tool, &[], cancel).unwrap(),
        );
        let formatted = "{\n  \"port\": 80\n}\n";
        assert_eq!(report.formatted.as_deref(), Some(formatted));
        let token = panel.request.as_ref().unwrap().token;
        assert!(matches!(
            panel.update(Message::Apply(token), Some(context), None, String::new),
            Update::Apply(text) if text.as_ref() == formatted
        ));
    }

    #[test]
    fn groovy_formatter_routes_snippet_and_sftp_file_types_to_review_and_apply() {
        let language = Language::named("groovy").expect("Groovy snippet language");
        for path in ["notes.groovy", "build.gradle", "script.gvy", "Jenkinsfile"] {
            assert!(Language::extension(Path::new(path)) == Some(language));
        }
        assert!(language.tool(Action::CheckSyntax).is_none());
        let (mut panel, context, report) = run_fixture(
            language,
            Action::Format,
            "\t  println 'café'",
            |tool, cancel| inspect_paths(tool, &[], cancel).unwrap(),
        );
        let formatted = "    println 'café'";
        assert_eq!(report.formatted.as_deref(), Some(formatted));
        let token = panel.request.as_ref().unwrap().token;
        assert!(matches!(
            panel.update(Message::Apply(token), Some(context), None, String::new),
            Update::Apply(text) if text.as_ref() == formatted
        ));
    }

    #[test]
    fn external_formatter_routes_use_fixed_stdin_arguments() {
        let formats = [
            ("file.xml", "xmllint", "-"),
            ("file.yml", "yamlfmt", "-in"),
            ("file.toml", "taplo", "-"),
            ("file.htm", "biome", "--stdin-file-path=buffer.html"),
            ("file.css", "biome", "--stdin-file-path=buffer.css"),
            ("file.mjs", "biome", "--stdin-file-path=buffer.js"),
            ("file.mts", "biome", "--stdin-file-path=buffer.ts"),
            ("file.java", "google-java-format", "-"),
            ("file.pm", "perltidy", "-se"),
            ("file.sql", "pg_format", "-"),
            ("file.tfvars", "terraform", "-"),
        ];
        for (path, program, last_argument) in formats {
            let language = Language::extension(Path::new(path)).unwrap();
            let tool = external(language.tool(Action::Format).unwrap());
            assert_eq!(tool.name, program);
            assert_eq!(tool.arguments.last(), Some(&last_argument));
            assert!(language.tool(Action::CheckSyntax).is_none());
        }
    }

    #[test]
    #[ignore = "requires a local PerlTidy executable or launcher in KORTTY_TEST_PERLTIDY_DIR"]
    fn editor_tool_native_perltidy() {
        let (_, _, report) = installed_formatter(
            Language::Perl,
            "KORTTY_TEST_PERLTIDY_DIR",
            "my $x=1; print $x;\n",
        );
        let formatted = report
            .formatted
            .as_deref()
            .unwrap_or_else(|| panic!("{}: {}", report.notice, report.diagnostics));
        assert!(formatted.contains("my $x = 1;"));
    }
}
