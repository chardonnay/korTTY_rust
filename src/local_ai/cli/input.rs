//! The literal argv and masked input approved for one configured CLI request.

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
    File(FileRole),
}

impl Argument {
    pub(super) fn review(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            Self::File(role) => role.placeholder(),
        }
    }

    fn resolve(&self, directory: &Path) -> Result<String, Error> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::File(role) => directory
                .join(role.name())
                .into_os_string()
                .into_string()
                .map_err(|_| {
                    Error::Review("The private CLI prompt file path is not UTF-8.".into())
                }),
        }
    }
}

pub(super) fn mode(arguments: &[String]) -> Option<Mode> {
    let mut counts = [0_usize; 4];
    for argument in arguments {
        match argument.as_str() {
            "{stdinPrompt}" => counts[0] += 1,
            "{promptFile}" => counts[1] += 1,
            "{systemPromptFile}" => counts[2] += 1,
            "{userPromptFile}" => counts[3] += 1,
            "{model}" => {}
            literal
                if [
                    "{stdinPrompt}",
                    "{promptFile}",
                    "{systemPromptFile}",
                    "{userPromptFile}",
                    "{prompt}",
                    "{reasoning}",
                    "{model}",
                ]
                .iter()
                .any(|placeholder| literal.contains(placeholder)) =>
            {
                return None;
            }
            _ => {}
        }
    }
    match counts {
        [0 | 1, 0, 0, 0] => Some(Mode::Stdin),
        [0, 1, 0, 0] => Some(Mode::Combined),
        [0, 0, 1, 1] => Some(Mode::Split),
        _ => None,
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
