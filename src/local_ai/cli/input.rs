//! The literal argv and masked input approved for one configured CLI request.

use std::borrow::Cow;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use zeroize::Zeroizing;

use super::{Error, MAX_ARG_BYTES, MAX_INPUT, Program, Scratch, active, nonce};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Mode {
    Stdin,
    Combined,
    Split,
    MiniMax,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum FileRole {
    Combined,
    System,
    User,
    EditorScript,
}

impl FileRole {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Combined => "prompt.txt",
            Self::System => "system-prompt.txt",
            Self::User => "user-prompt.txt",
            Self::EditorScript => super::EDITOR_SCRIPT,
        }
    }

    pub(super) const fn placeholder(self) -> &'static str {
        match self {
            Self::Combined => "{promptFile}",
            Self::System => "{systemPromptFile}",
            Self::User => "{userPromptFile}",
            Self::EditorScript => super::EDITOR_SCRIPT,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) enum Argument {
    Literal(String),
    Template(Vec<Part>),
}

impl Argument {
    pub(super) fn review(&self) -> Cow<'_, str> {
        match self {
            Self::Literal(value) => Cow::Borrowed(value),
            Self::Template(parts) => Cow::Owned(parts.iter().map(Part::review).collect()),
        }
    }

    fn resolve(&self, directory: &Path) -> Result<String, Error> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Template(parts) => {
                let mut argument = String::new();
                for part in parts {
                    match part {
                        Part::Literal(value) => argument.push_str(value),
                        Part::File(role) => argument.push_str(
                            directory.join(role.name()).to_str().ok_or_else(|| {
                                Error::Review(
                                    "The private CLI prompt file path is not UTF-8.".into(),
                                )
                            })?,
                        ),
                    }
                }
                Ok(argument)
            }
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) enum Part {
    Literal(String),
    File(FileRole),
}

impl Part {
    fn review(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            Self::File(role) => role.placeholder(),
        }
    }
}

#[derive(Clone, Copy)]
enum Atom<'a> {
    Literal(&'a str),
    Model,
    Reasoning,
    File(FileRole),
    Unsupported,
}

fn atoms(argument: &str) -> impl Iterator<Item = Atom<'_>> {
    argument.split_inclusive('}').flat_map(|chunk| {
        let token = chunk.rfind('{').and_then(|start| {
            let atom = match &chunk[start..] {
                super::MODEL => Atom::Model,
                super::REASONING => Atom::Reasoning,
                "{promptFile}" => Atom::File(FileRole::Combined),
                "{systemPromptFile}" => Atom::File(FileRole::System),
                "{userPromptFile}" => Atom::File(FileRole::User),
                "{stdinPrompt}" | "{prompt}" => Atom::Unsupported,
                _ => return None,
            };
            Some((start, atom))
        });
        if let Some((start, atom)) = token {
            [
                (start > 0).then(|| Atom::Literal(&chunk[..start])),
                Some(atom),
            ]
        } else {
            [Some(Atom::Literal(chunk)), None]
        }
        .into_iter()
        .flatten()
    })
}

pub(super) struct Template {
    pub(super) mode: Mode,
    pub(super) uses_model: bool,
    pub(super) uses_reasoning: bool,
    pub(super) explicit_prompt: bool,
}

impl Template {
    pub(super) fn uses_reasoning(arguments: &[String]) -> bool {
        arguments
            .iter()
            .flat_map(|argument| atoms(argument))
            .any(|atom| matches!(atom, Atom::Reasoning))
    }

    pub(super) fn parse(arguments: &[String]) -> Option<Self> {
        let mut counts = [0_usize; 4];
        let mut uses_model = false;
        let mut uses_reasoning = false;
        for argument in arguments {
            if argument == "{stdinPrompt}" {
                counts[0] += 1;
                continue;
            }
            for atom in atoms(argument) {
                match atom {
                    Atom::Model => uses_model = true,
                    Atom::Reasoning => uses_reasoning = true,
                    Atom::File(FileRole::Combined) => counts[1] += 1,
                    Atom::File(FileRole::System) => counts[2] += 1,
                    Atom::File(FileRole::User) => counts[3] += 1,
                    Atom::Unsupported => return None,
                    Atom::Literal(_) | Atom::File(FileRole::EditorScript) => {}
                }
            }
        }
        let mode = match counts {
            [0 | 1, 0, 0, 0] => Mode::Stdin,
            [0, combined, 0, 0] if combined > 0 => Mode::Combined,
            [0, 0, system, user] if system > 0 && user > 0 => Mode::Split,
            _ => return None,
        };
        Some(Self {
            mode,
            uses_model,
            uses_reasoning,
            explicit_prompt: counts != [0; 4],
        })
    }

    pub(super) fn expand(arguments: &[String], model: &str, reasoning: &str) -> Vec<Argument> {
        arguments
            .iter()
            .filter(|argument| argument.as_str() != "{stdinPrompt}")
            .map(|argument| {
                let mut file = false;
                let parts: Vec<_> = atoms(argument)
                    .map(|atom| match atom {
                        Atom::Literal(value) => Part::Literal(value.into()),
                        Atom::Model => Part::Literal(model.into()),
                        Atom::Reasoning => Part::Literal(reasoning.into()),
                        Atom::File(role) => {
                            file = true;
                            Part::File(role)
                        }
                        Atom::Unsupported => unreachable!("validated CLI template"),
                    })
                    .collect();
                if file {
                    Argument::Template(parts)
                } else {
                    Argument::Literal(parts.iter().map(Part::review).collect())
                }
            })
            .collect()
    }
}

#[derive(Clone)]
pub(crate) struct Request {
    program: Program,
    pub(super) name: String,
    directory: PathBuf,
    pub(super) launch: Launch,
    pub(super) payload: Payload,
}

#[derive(Clone)]
pub(super) struct Launch {
    pub(super) path: PathBuf,
    pub(super) arguments: Vec<String>,
}

#[derive(Clone)]
pub(super) enum Payload {
    Stdin(Zeroizing<String>),
    EditorScript {
        input: Zeroizing<String>,
        script: Zeroizing<String>,
    },
    Combined(Zeroizing<String>),
    MiniMax(Zeroizing<String>),
    Split {
        system: Zeroizing<String>,
        user: Zeroizing<String>,
    },
}

impl Request {
    pub(in crate::local_ai) fn prepare(
        program: &Program,
        root: &Path,
        system: &str,
        history: &[(&str, &str)],
        input: &str,
    ) -> Result<Self, Error> {
        let payload = match program.mode {
            Mode::Stdin => Payload::Stdin(super::prompt(system, history, input)?),
            Mode::Combined => Payload::Combined(super::prompt(system, history, input)?),
            Mode::MiniMax => {
                let messages = Zeroizing::new(super::super::preview_chat(system, history, input)?);
                if messages.len() > MAX_INPUT {
                    return Err(Error::Invalid("the CLI request exceeds 1 MiB"));
                }
                Payload::MiniMax(messages)
            }
            Mode::Split => {
                let user = split_user(history, input)?;
                if system.len().saturating_add(user.len()) > MAX_INPUT {
                    return Err(Error::Invalid("the CLI request exceeds 1 MiB"));
                }
                Payload::Split {
                    system: Zeroizing::new(system.into()),
                    user,
                }
            }
        };
        Self::with_payload(program, root, payload)
    }

    pub(super) fn without_input(program: &Program, root: &Path) -> Result<Self, Error> {
        Self::with_payload(program, root, Payload::Stdin(Zeroizing::new(String::new())))
    }

    pub(super) fn editor(
        program: &Program,
        root: &Path,
        source: &str,
        script: Option<&str>,
    ) -> Result<Self, Error> {
        if program.mode != Mode::Stdin || source.contains('\0') {
            return Err(Error::Invalid(
                "The editor tool requires UTF-8 source on stdin.",
            ));
        }
        let input = Zeroizing::new(source.into());
        let payload = if let Some(script) = script {
            Payload::EditorScript {
                input,
                script: Zeroizing::new(script.into()),
            }
        } else {
            Payload::Stdin(input)
        };
        Self::with_payload(program, root, payload)
    }

    fn with_payload(program: &Program, root: &Path, payload: Payload) -> Result<Self, Error> {
        let name = format!(".ai-cli-{}", nonce().map_err(|_| Error::Service)?);
        let directory = root.join(&name);
        let arguments = program
            .arguments
            .iter()
            .map(|argument| argument.resolve(&directory))
            .collect::<Result<Vec<_>, _>>()?;
        if arguments.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES {
            return Err(Error::Invalid("expanded CLI arguments exceed 8 KiB"));
        }
        Ok(Self {
            program: program.clone(),
            name,
            directory,
            launch: Launch {
                path: program.path.clone(),
                arguments,
            },
            payload,
        })
    }

    pub(super) fn matches(&self, program: &Program, root: &Path) -> bool {
        self.program == *program && self.directory.parent() == Some(root)
    }

    pub(crate) fn command_review(&self) -> String {
        self.program.review_arguments(&self.launch.arguments)
    }

    pub(crate) fn reviewed_arguments(&self) -> impl Iterator<Item = &str> {
        // MiniMax has a fixed bundled bootstrap, with no configured arguments.
        let arguments = if self.program.minimax.is_some() {
            &[][..]
        } else {
            self.launch.arguments.as_slice()
        };
        arguments.iter().map(String::as_str)
    }

    pub(crate) fn primary(&self) -> (&'static str, &str) {
        match &self.payload {
            Payload::Stdin(input) | Payload::EditorScript { input, .. } => {
                ("Exact UTF-8 stdin", input)
            }
            Payload::Combined(input) => ("Exact UTF-8 prompt.txt", input),
            Payload::MiniMax(input) => ("Exact MiniMax JSON messages on stdin", input),
            Payload::Split { system, .. } => ("Exact UTF-8 system-prompt.txt", system),
        }
    }

    pub(crate) fn secondary(&self) -> Option<&str> {
        match &self.payload {
            Payload::Split { user, .. } => Some(user),
            _ => None,
        }
    }

    pub(crate) fn uses_files(&self) -> bool {
        !matches!(self.payload, Payload::Stdin(_) | Payload::MiniMax(_))
    }
}

impl Payload {
    pub(super) fn take_stdin(&mut self) -> Zeroizing<String> {
        match self {
            Self::Stdin(input) | Self::MiniMax(input) | Self::EditorScript { input, .. } => {
                std::mem::replace(input, Zeroizing::new(String::new()))
            }
            _ => Zeroizing::new(String::new()),
        }
    }

    pub(super) fn publish(
        &self,
        scratch: &Scratch,
        deadline: Instant,
        canceled: &AtomicBool,
    ) -> Result<(), Error> {
        let directory = scratch
            .directory
            .as_ref()
            .expect("owned private working directory");
        let check = || {
            active(deadline, canceled).map_err(|_| {
                io::Error::new(io::ErrorKind::Interrupted, "CLI prompt staging stopped")
            })
        };
        for (role, contents) in self.files().into_iter().flatten() {
            active(deadline, canceled)?;
            let result = directory.publish_checked(
                role.name(),
                |file| file.write_all(contents.as_bytes()),
                check,
            );
            active(deadline, canceled)?;
            result.map_err(|_| {
                Error::Review("Could not stage the exact reviewed CLI prompt files.".into())
            })?;
        }
        Ok(())
    }

    pub(super) fn verify(&self, scratch: &Scratch) -> Result<(), Error> {
        let directory = scratch
            .directory
            .as_ref()
            .expect("owned private working directory");
        let verify = || {
            for (role, contents) in self.files().into_iter().flatten() {
                let file = directory.open_private_file(role.name())?;
                crate::private_files::require_single_link(&file)?;
                let mut bytes = Zeroizing::new(Vec::new());
                file.take(contents.len() as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.as_slice() != contents.as_bytes() {
                    return Err(io::Error::other("The staged CLI prompt changed"));
                }
            }
            Ok(())
        };
        verify()
            .map_err(|_| Error::Review("A private CLI prompt file changed before launch.".into()))
    }

    fn files(&self) -> [Option<(FileRole, &str)>; 2] {
        match self {
            Self::Stdin(_) | Self::MiniMax(_) => [None, None],
            Self::EditorScript { script, .. } => [Some((FileRole::EditorScript, script)), None],
            Self::Combined(input) => [Some((FileRole::Combined, input)), None],
            Self::Split { system, user } => [
                Some((FileRole::System, system)),
                Some((FileRole::User, user)),
            ],
        }
    }
}

fn split_user(history: &[(&str, &str)], input: &str) -> Result<Zeroizing<String>, Error> {
    use std::fmt::Write as _;

    if history.is_empty() {
        return Ok(Zeroizing::new(input.into()));
    }
    let history = Zeroizing::new(serde_json::to_string(history).map_err(|_| Error::Service)?);
    let mut user = Zeroizing::new(String::new());
    write!(
        user,
        "Conversation history (ordered user and assistant pairs):\n{}\n\nUser prompt:\n{input}\n",
        history.as_str(),
    )
    .expect("writing to a string");
    Ok(user)
}
