use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, Weak};

use iced::Task;
use kortty_vault::{Credential, Secret, sudo_scope};
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::local_ai::cli::{self, Program};
use crate::ssh_dialog::LoginTarget;

use super::{Message, PasswordCommandInspection};

pub(super) const MAX_ARGUMENT_INPUT: usize = 16 * 1024;

#[derive(Deserialize, Serialize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
struct Recipe {
    executable: String,
    arguments: Vec<String>,
    fingerprint: [u8; 32],
}

impl Recipe {
    fn program(&self) -> Result<Program, String> {
        cli::password_program(
            PathBuf::from(&self.executable),
            &self.arguments,
            self.fingerprint,
        )
    }
}

pub(crate) struct CommandAuthority {
    program: Program,
    revision: Weak<()>,
}

impl CommandAuthority {
    pub(super) fn new(recipe: &str, revision: Weak<()>) -> Result<Self, String> {
        let recipe: Recipe = serde_json::from_str(recipe)
            .map_err(|_| "The saved password command must be reviewed again in the vault.")?;
        Ok(Self {
            program: recipe.program()?,
            revision,
        })
    }

    pub(crate) fn review(&self) -> String {
        self.program.command_review()
    }

    pub(crate) fn current(&self) -> bool {
        self.revision.strong_count() != 0
    }

    pub(crate) fn run(
        &self,
        root: &Path,
        cancel: &AtomicBool,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Secret, String> {
        let review = || {
            if !self.current() {
                return Err("The vault or password command changed.".into());
            }
            authorize()
        };
        let result = cli::run_password_program(&self.program, root, cancel, review)?;
        review()?;
        Ok(result)
    }
}

pub(super) fn matches_endpoint(entry: &Credential, endpoint: &LoginTarget) -> bool {
    entry.username == endpoint.username
        && sudo_scope(&entry.server_pattern).is_some_and(|(host, port)| {
            port == endpoint.port
                && crate::ssh_dialog::normalize_host(host).as_deref() == Ok(endpoint.host.as_str())
        })
}

pub(super) fn inspect(executable: Secret, arguments: Secret, revision: Arc<()>) -> Task<Message> {
    let (send, receive) = async_channel::bounded(1);
    let worker = std::thread::Builder::new()
        .name("kortty-password-program-review".into())
        .spawn(move || {
            let _ = send.try_send(inspect_recipe(&executable, &arguments));
        });
    Task::future(async move {
        let result = match worker {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("Could not inspect the password program.".into())),
            Err(_) => Err("Could not inspect the password program.".into()),
        };
        Message::PasswordCommandInspected(PasswordCommandInspection(
            revision,
            Arc::new(Mutex::new(Some(result))),
        ))
    })
}

fn inspect_recipe(executable: &str, arguments: &str) -> Result<Secret, String> {
    if executable.len() > 4096 || arguments.len() > MAX_ARGUMENT_INPUT {
        return Err("The executable path or arguments exceed their size limit.".into());
    }
    let arguments = if arguments.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(arguments)
            .map_err(|_| "Enter arguments as a JSON array of strings.")?
    };
    let (path, fingerprint) = Program::inspect(Path::new(executable))?;
    let executable = path
        .into_os_string()
        .into_string()
        .map_err(|_| "The password program requires a UTF-8 path.")?;
    let recipe = Recipe {
        executable,
        arguments,
        fingerprint,
    };
    recipe.program()?;
    serde_json::to_string(&recipe)
        .map(Secret::new)
        .map_err(|_| "Could not save the reviewed password program.".into())
}

#[cfg(test)]
#[test]
fn ssh_password_command_vault_record_is_private_typed_and_revoked_by_lock() {
    use kortty_vault::{CredentialInput, CredentialKind, Vault};

    let root = crate::snippets::tests::Profile::new();
    let path = root.0.join("command-vault.bin");
    let mut vault = Vault::create(&path, b"password-command-fixture-master").unwrap();
    let executable = root.0.join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    let recipe = Recipe {
        executable: executable.to_str().unwrap().into(),
        arguments: vec!["private-literal-argument".into()],
        fingerprint: [7; 32],
    };
    let id = vault
        .save(
            CredentialInput {
                label: "Fixture helper".into(),
                username: "operator".into(),
                server_pattern: "example.test:2222".into(),
                kind: CredentialKind::SshPasswordCommand,
                ..Default::default()
            },
            Secret::new(serde_json::to_string(&recipe).unwrap()),
        )
        .unwrap();
    drop(vault);
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows(b"private-literal-argument".len())
            .any(|part| part == b"private-literal-argument")
    );
    let vault = Vault::unlock(&path, b"password-command-fixture-master").unwrap();
    let mut window = super::VaultWindow::with_unlocked_fixture(path, vault);
    let mut endpoint = LoginTarget {
        host: "example.test".into(),
        port: 2222,
        username: "operator".into(),
    };
    assert!(
        window
            .resolve(&id, super::Kind::Password, &endpoint)
            .is_err()
    );
    let command = window.password_command(&id, &endpoint).unwrap().unwrap();
    assert!(command.current());
    endpoint.port = 22;
    assert!(window.password_command(&id, &endpoint).is_err());
    let _ = window.update(Message::Lock);
    assert!(!command.current());
}
