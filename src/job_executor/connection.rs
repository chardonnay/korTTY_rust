use std::path::PathBuf;
use std::sync::Arc;

use kortty_ssh::{Authentication, Endpoint, SshTarget};
use kortty_vault::Secret;

use crate::clipboard_policy::ServerAccess;
#[cfg(test)]
use crate::connection_store::Auth;
use crate::connection_store::{ActiveAuth, Connection, SavedLogin, Store};
use crate::scheduler_window::{ConnectionRef, SshCommand};
use crate::ssh_connection;
use crate::ssh_dialog::{LoginTarget, Password};
use crate::ssh_key_catalog::Catalog;
use crate::vault_window::Kind;

use super::Failure;

const SERVER_BLOCKED: &str = "Managed policy blocks this SSH server or jump host";

pub(crate) type SavedAuth = Result<Option<Credentials>, &'static str>;
pub(crate) type BatchAuth = Result<Vec<Result<Credentials, &'static str>>, &'static str>;
pub(super) type KeyCatalog = Option<Result<Arc<Catalog>, &'static str>>;

pub(crate) enum JobAuth {
    Manual,
    Saved(Result<Box<Credentials>, &'static str>),
    Batch(BatchAuth),
}

impl JobAuth {
    pub(crate) fn with_sudo_passwords(
        self,
        mut password: impl FnMut(&ConnectionRef) -> Result<Secret, &'static str>,
    ) -> Self {
        match self {
            Self::Manual => Self::Manual,
            Self::Saved(saved) => Self::Saved(saved.and_then(|mut credentials| {
                credentials.sudo_password = Some(password(&credentials.reference)?);
                Ok(credentials)
            })),
            Self::Batch(saved) => Self::Batch(saved.map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| {
                        entry.and_then(|mut credentials| {
                            credentials.sudo_password = Some(password(&credentials.reference)?);
                            Ok(credentials)
                        })
                    })
                    .collect()
            })),
        }
    }
}

pub(crate) struct Credentials {
    reference: ConnectionRef,
    target: Login,
    jump: Option<Login>,
    server_access: ServerAccess,
    sudo_password: Option<Secret>,
}

struct Login {
    endpoint: LoginTarget,
    authentication: LoginAuth,
}

enum LoginAuth {
    Password(Secret),
    Key(PathBuf, Option<Secret>),
    StoredKey {
        catalog: Arc<Catalog>,
        id: u64,
        passphrase: Option<Secret>,
    },
}

impl Credentials {
    pub(super) fn take_sudo_password(&mut self) -> Option<Secret> {
        self.sudo_password.take()
    }

    pub(crate) fn add_journal_secrets(&self, secrets: &crate::terminal_logging::Secrets) {
        if let Some(password) = &self.sudo_password {
            secrets.add(password.as_str());
        }
        for login in std::iter::once(&self.target).chain(self.jump.iter()) {
            let secret = match &login.authentication {
                LoginAuth::Password(secret) => Some(secret),
                LoginAuth::Key(_, passphrase) | LoginAuth::StoredKey { passphrase, .. } => {
                    passphrase.as_ref()
                }
            };
            if let Some(secret) = secret {
                secrets.add(secret.as_str());
            }
        }
    }

    pub(crate) fn resolve_directory_browser(
        reference: &ConnectionRef,
        current: &Connection,
        server_access: &ServerAccess,
        secret: impl FnMut(&str, Kind, &LoginTarget) -> Result<Secret, &'static str>,
    ) -> Result<Self, &'static str> {
        Self::resolve_for(
            Some(reference),
            current,
            false,
            &mut None,
            server_access,
            secret,
        )
    }

    pub(super) fn into_route(self) -> Result<(SshTarget, Vec<SshTarget>), Failure> {
        let target = self.target.endpoint()?;
        let jump = self.jump.as_ref().map(Login::endpoint).transpose()?;
        if jump
            .as_ref()
            .is_some_and(|jump| !endpoint_allowed(jump, &self.server_access))
            || !endpoint_allowed(&target, &self.server_access)
        {
            return Err(Failure::blocked(SERVER_BLOCKED));
        }
        Ok((
            self.target.load(target)?,
            self.jump
                .zip(jump)
                .map(|(login, endpoint)| login.load(endpoint))
                .transpose()?
                .into_iter()
                .collect(),
        ))
    }

    pub(crate) fn into_swarm_route(self) -> Result<(SshTarget, Vec<SshTarget>), &'static str> {
        self.into_route().map_err(|failure| failure.detail)
    }

    pub(crate) fn resolve_batch(
        action: &SshCommand,
        store: &Store,
        server_access: &ServerAccess,
        mut secret: impl FnMut(&str, Kind, &LoginTarget) -> Result<Secret, &'static str>,
    ) -> BatchAuth {
        action.validate_target_selection()?;
        let profiles = action
            .targets()
            .map(|reference| {
                let profile = store
                    .get(reference.id)
                    .ok_or("A selected SSH connection is missing.")?;
                if ConnectionRef::from(profile) != *reference {
                    return Err(
                        "A selected SSH route changed. Choose the connection again in this job.",
                    );
                }
                Ok((reference, profile))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut keys = None;
        Ok(profiles
            .into_iter()
            .map(|(reference, profile)| {
                Self::resolve_for(
                    Some(reference),
                    profile,
                    false,
                    &mut keys,
                    server_access,
                    &mut secret,
                )
            })
            .collect())
    }

    pub(crate) fn resolve(
        action: &SshCommand,
        current: &Connection,
        server_access: &ServerAccess,
        secret: impl FnMut(&str, Kind, &LoginTarget) -> Result<Secret, &'static str>,
    ) -> Result<Self, &'static str> {
        Self::resolve_for(
            action.connection.as_ref(),
            current,
            action.virtual_terminal.is_some(),
            &mut None,
            server_access,
            secret,
        )
    }

    pub(super) fn resolve_for(
        reviewed: Option<&ConnectionRef>,
        current: &Connection,
        recording: bool,
        keys: &mut KeyCatalog,
        server_access: &ServerAccess,
        mut secret: impl FnMut(&str, Kind, &LoginTarget) -> Result<Secret, &'static str>,
    ) -> Result<Self, &'static str> {
        let reference = ConnectionRef::from(current);
        if reviewed != Some(&reference) {
            return Err("The saved SSH route changed. Choose the connection again in this job.");
        }
        if !reference_allowed(&reference, server_access) {
            return Err(SERVER_BLOCKED);
        }
        if current.incognito || !current.isolation.is_none() {
            return Err("This saved connection requires privacy or isolation unavailable to jobs.");
        }
        if let Some(problem) = current.missing_username() {
            return Err(problem);
        }
        for login in std::iter::once(&current.login).chain(current.jump.iter()) {
            if login.endpoint.validate().is_err() {
                return Err("The saved SSH login needs a valid username before this job can run.");
            }
            if login.keyboard_interactive || login.auth.requires_interactive() {
                return Err("The saved connection requires an interactive SSH login.");
            }
            if recording && matches!(login.auth.active(), ActiveAuth::VaultPassword(_)) {
                return Err("An SSH password job cannot record a virtual terminal.");
            }
        }
        let mut resolve = |login: &SavedLogin| {
            let endpoint = &login.endpoint;
            let authentication = match login.auth.active() {
                ActiveAuth::VaultPassword(id) => {
                    LoginAuth::Password(secret(id, Kind::Password, endpoint)?)
                }
                ActiveAuth::KeyFile { path, passphrase } => LoginAuth::Key(
                    path.to_owned(),
                    passphrase
                        .map(|id| secret(id, Kind::KeyPassphrase, endpoint))
                        .transpose()?,
                ),
                ActiveAuth::StoredKey(id) => {
                    let catalog = key_catalog(keys)?;
                    let key = catalog.get(id).ok_or("A registered SSH key is missing.")?;
                    let passphrase = key
                        .passphrase
                        .as_deref()
                        .map(|id| secret(id, Kind::KeyPassphrase, endpoint))
                        .transpose()?;
                    LoginAuth::StoredKey {
                        catalog: Arc::clone(catalog),
                        id,
                        passphrase,
                    }
                }
                ActiveAuth::Prompt | ActiveAuth::KeyboardInteractive => {
                    return Err("The saved connection requires an interactive SSH login.");
                }
            };
            Ok(Login {
                endpoint: endpoint.clone(),
                authentication,
            })
        };
        Ok(Self {
            reference,
            target: resolve(&current.login)?,
            jump: current.jump.as_ref().map(resolve).transpose()?,
            server_access: server_access.clone(),
            sudo_password: None,
        })
    }
}

fn key_catalog(keys: &mut KeyCatalog) -> Result<&Arc<Catalog>, &'static str> {
    keys.get_or_insert_with(|| {
        Catalog::open_profile()
            .map(Arc::new)
            .map_err(|_| "The registered SSH keys are unavailable.")
    })
    .as_ref()
    .map_err(|&error| error)
}

pub(super) fn route(
    action: &SshCommand,
    credentials: Option<Credentials>,
    server_access: &ServerAccess,
) -> Result<(SshTarget, Vec<SshTarget>), Failure> {
    match (action.connection.as_ref(), credentials) {
        (Some(reference), Some(credentials)) if *reference == credentials.reference => {
            credentials.into_route()
        }
        (Some(_), None) => Err(Failure::blocked("The saved SSH connection is unavailable.")),
        (_, Some(_)) => Err(Failure::blocked("Unexpected saved SSH credentials.")),
        (None, None) => Ok((
            SshTarget {
                endpoint: manual_endpoint(action, server_access)?,
                username: action.username.clone(),
                authentication: ssh_connection::private_key(&action.key_file)
                    .map_err(|_| Failure::failed("Could not read the SSH private key"))?,
            },
            Vec::new(),
        )),
    }
}

pub(super) fn reference_allowed(reference: &ConnectionRef, server_access: &ServerAccess) -> bool {
    let route = reference
        .jump
        .iter()
        .chain(std::iter::once(&reference.target));
    server_access
        .first_blocked(route.map(|login| (login.host.as_str(), login.port)))
        .is_none()
}

pub(super) fn check_action_route(
    action: &SshCommand,
    server_access: &ServerAccess,
) -> Result<(), Failure> {
    match &action.connection {
        Some(reference) if reference_allowed(reference, server_access) => Ok(()),
        Some(_) => Err(Failure::blocked(SERVER_BLOCKED)),
        None => manual_endpoint(action, server_access).map(|_| ()),
    }
}

fn manual_endpoint(action: &SshCommand, server_access: &ServerAccess) -> Result<Endpoint, Failure> {
    let endpoint = Endpoint::new(&action.host, action.port)
        .map_err(|_| Failure::blocked("Invalid SSH endpoint"))?;
    if !endpoint_allowed(&endpoint, server_access) {
        return Err(Failure::blocked(SERVER_BLOCKED));
    }
    Ok(endpoint)
}

fn endpoint_allowed(endpoint: &Endpoint, server_access: &ServerAccess) -> bool {
    server_access.allows(endpoint.wire_host().as_ref(), endpoint.port())
}

impl Login {
    fn endpoint(&self) -> Result<Endpoint, Failure> {
        Endpoint::new(&self.endpoint.host, self.endpoint.port)
            .map_err(|_| Failure::blocked("Invalid saved SSH endpoint"))
    }

    fn load(self, endpoint: Endpoint) -> Result<SshTarget, Failure> {
        let authentication = match self.authentication {
            LoginAuth::Password(secret) => {
                Authentication::password(Password::from_secret(secret).into_string())
            }
            LoginAuth::Key(path, Some(secret)) => {
                ssh_connection::private_key_with_passphrase(&path, Password::from_secret(secret))
                    .map_err(|_| Failure::failed("Could not read the saved SSH private key"))?
            }
            LoginAuth::Key(path, None) => ssh_connection::private_key(&path)
                .map_err(|_| Failure::failed("Could not read the saved SSH private key"))?,
            LoginAuth::StoredKey {
                catalog,
                id,
                passphrase,
            } => {
                let unavailable =
                    || Failure::blocked("A registered SSH key changed or is unavailable.");
                let key = catalog.get(id).ok_or_else(unavailable)?;
                // read_key checks the current disk record before and after reading the key.
                let mut bytes = catalog.read_key(key).map_err(|_| unavailable())?;
                let bytes = std::mem::take(&mut *bytes);
                match passphrase {
                    Some(secret) => Authentication::private_key_with_passphrase(
                        bytes,
                        Password::from_secret(secret).into_string(),
                    ),
                    None => Authentication::private_key(bytes),
                }
            }
        };
        Ok(SshTarget {
            endpoint,
            username: self.endpoint.username,
            authentication,
        })
    }
}

#[cfg(test)]
mod tests {
    use kortty_vault::{CredentialInput, Vault};

    use super::*;
    use crate::job_executor::{
        ExecutionPolicy, batch, open_directory_browser, run,
        tests::{blocked_server_policy, scheduled_request},
        virtual_terminal,
    };
    use crate::session_security::IsolationLevel;
    use crate::vault_window::{VaultWindow, resolve_credential};

    fn saved_profile() -> Connection {
        let endpoint = LoginTarget {
            host: "target.example.test".into(),
            port: 22,
            username: "operator".into(),
        };
        Connection {
            id: 1,
            name: "Saved target".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint,
                auth: Auth::VaultPassword("test-reference".into()),
                keyboard_interactive: false,
            },
            jump: None,
            emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: IsolationLevel::None,
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        }
    }

    #[test]
    fn saved_job_rejects_stale_routes_interaction_and_locked_vault_before_authentication() {
        let mut profile = saved_profile();
        let endpoint = profile.login.endpoint.clone();
        let (_, mut request) = scheduled_request();
        request.action = std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({
                "connection": ConnectionRef::from(&profile), "command": "true",
            }))
            .unwrap(),
        );
        let should_not_lookup =
            |_: &str, _, _: &LoginTarget| panic!("a rejected job must not request a vault secret");
        profile.login.endpoint.port = 2222;
        assert!(
            matches!(Credentials::resolve(&request.action, &profile, &ServerAccess::default(), should_not_lookup), Err(error) if error.contains("route changed"))
        );
        profile.login.endpoint = endpoint;
        profile.login.keyboard_interactive = true;
        assert!(
            matches!(Credentials::resolve(&request.action, &profile, &ServerAccess::default(), should_not_lookup), Err(error) if error.contains("interactive"))
        );
        profile.login.keyboard_interactive = false;
        profile.isolation = IsolationLevel::Process;
        assert!(
            matches!(Credentials::resolve(&request.action, &profile, &ServerAccess::default(), should_not_lookup), Err(error) if error.contains("isolation"))
        );
        profile.isolation = IsolationLevel::None;
        let mut recording = request.action.as_ref().clone();
        recording.virtual_terminal = Some(virtual_terminal::Options::default());
        assert!(
            matches!(Credentials::resolve(&recording, &profile, &ServerAccess::default(), should_not_lookup), Err(error) if error.contains("cannot record"))
        );

        let temporary = crate::snippets::tests::Profile::new();
        let path = temporary.0.join("vault");
        let vault = VaultWindow::open(path.clone()).unwrap();
        let saved = Credentials::resolve(
            &request.action,
            &profile,
            &ServerAccess::default(),
            |id, kind, endpoint| vault.resolve(id, kind, endpoint),
        )
        .map(Some);
        let completion = run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(true),
            saved,
            Ok(None),
            || panic!("a locked vault must not start SSH setup"),
        );
        assert_eq!(completion.outcome, kortty_jobs::RunOutcome::Blocked);
        assert_eq!(
            completion.detail.as_deref(),
            Some("Unlock the vault before connecting.")
        );
        let mut vault = Vault::create(&path, b"scheduler-test-master-password").unwrap();
        let mut input = CredentialInput {
            label: "Scoped password".into(),
            username: "operator".into(),
            server_pattern: "wrong.example.test".into(),
            ..CredentialInput::default()
        };
        let secret = || Secret::new("scheduler-test-password".into());
        let id = vault.save(input.clone(), secret()).unwrap();
        profile.login.auth = Auth::VaultPassword(id.clone());
        let resolve = |vault: &Vault| {
            Credentials::resolve(
                &request.action,
                &profile,
                &ServerAccess::default(),
                |id, kind, endpoint| resolve_credential(vault, id, kind, endpoint),
            )
        };
        assert!(matches!(resolve(&vault), Err(error) if error.contains("outside the scope")));
        input.id = Some(id);
        input.server_pattern = "target.example.test".into();
        vault.save(input, secret()).unwrap();
        let password = resolve(&vault).unwrap();
        assert!(
            matches!(password.target.authentication, LoginAuth::Password(value) if value.as_str() == "scheduler-test-password")
        );
    }

    #[test]
    fn saved_jump_policy_blocks_before_vault_and_is_reported_per_target() {
        let mut blocked = saved_profile();
        blocked.id = 0;
        let mut jump = blocked.login.clone();
        jump.endpoint.host = "jump.example.test.".into();
        jump.endpoint.port = 2222;
        blocked.jump = Some(jump);
        let mut other = saved_profile();
        other.id = 0;
        other.login.auth = Auth::Prompt;
        let directory = crate::snippets::tests::Profile::new();
        let mut store = Store::open(directory.0.join("connections.json")).unwrap();
        let blocked = store.save(blocked).unwrap();
        let other = store.save(other).unwrap();
        let blocked = store.get(blocked).unwrap();
        let reference = ConnectionRef::from(blocked);
        let policy = blocked_server_policy("jump.example.test.:2222");
        let secret =
            |_: &str, _: Kind, _: &LoginTarget| panic!("do not load blocked SSH credentials");
        let (_, mut request) = scheduled_request();
        request.action = std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({
                "connection": reference,
                "additional_connections": [ConnectionRef::from(store.get(other).unwrap())],
                "command": "true",
            }))
            .unwrap(),
        );

        assert!(matches!(
            Credentials::resolve(&request.action, blocked, &policy.server_access, secret),
            Err(SERVER_BLOCKED)
        ));
        assert!(matches!(
            Credentials::resolve_directory_browser(
                &reference,
                blocked,
                &policy.server_access,
                secret
            ),
            Err(SERVER_BLOCKED)
        ));
        let credentials =
            Credentials::resolve_batch(&request.action, &store, &policy.server_access, secret);
        let completion = batch::run(&request, None, &policy, credentials, Ok(None), || {
            panic!("neither target can reach the SSH trust store")
        });
        assert_eq!(completion.outcome, kortty_jobs::RunOutcome::Blocked);
        let detail = completion.detail.unwrap();
        assert!(detail.contains("2 blocked") && detail.contains(SERVER_BLOCKED));
        assert!(detail.contains("interactive SSH login"));
    }

    #[test]
    fn standalone_browser_checks_its_cached_route_before_reading_a_key() {
        let mut profile = saved_profile();
        profile.login.auth = Auth::KeyFile {
            path: crate::snippets::tests::Profile::new().0.join("missing-key"),
            passphrase: None,
        };
        let reference = ConnectionRef::from(&profile);
        let mut credentials = Credentials::resolve_directory_browser(
            &reference,
            &profile,
            &ServerAccess::default(),
            |_, _, _| panic!("no passphrase"),
        )
        .unwrap();
        credentials.server_access = blocked_server_policy("target.example.test").server_access;
        assert!(matches!(
            open_directory_browser(
                credentials,
                || false,
                || { panic!("blocked standalone browser must not reach host-key trust") }
            ),
            Err(SERVER_BLOCKED)
        ));
    }
}
