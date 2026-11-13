//! Saved swarm targets resolve their route and vault credentials at approval.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use iced::{Task, window};

use crate::clipboard_policy::{AiAgentExecution, ClipboardPolicy};
use crate::connection_store::{Connection, SavedLogin, Store};
use crate::job_executor::Credentials;
use crate::scheduler_window::ConnectionRef;
use crate::terminal_agent::design::Preset as AgentDesign;
use crate::terminal_agent::swarm::headless::{Execution, Review};
use crate::terminal_logging::{Redactor, Secrets};

use super::super::{Binding, MAX_RUNS, Phase, ProfileChoice, Purpose, Run, RunSetup, mask};
use super::{
    Desktop, Message as GroupMessage, Mode, RunMessage, SwarmMode, Workspace, ai_mode, snippet,
    swarm_profiles,
};

mod model_review;
pub(in crate::workspace::terminal_agent) use model_review::ModelReview;

#[derive(Clone)]
pub struct Source {
    pub(in crate::workspace::terminal_agent) reference: ConnectionRef,
    pub(in crate::workspace::terminal_agent) label: String,
    pub(in crate::workspace::terminal_agent) directory: String,
    pub(super) group: Option<String>,
    security: SecurityRevision,
    store_path: PathBuf,
    follow_up: Option<FollowUp>,
}

#[derive(Clone)]
struct FollowUp {
    review: Arc<Review>,
    profile: ProfileChoice,
    profile_revision: [u8; 32],
    redactor: Redactor,
    vault_revision: Option<Weak<()>>,
}

#[derive(Clone, Eq, PartialEq)]
struct SecurityRevision {
    login: SavedLogin,
    jump: Option<SavedLogin>,
    incognito: bool,
    isolation: crate::session_security::IsolationLevel,
    startup: kortty_ssh::ConnectOptions,
    agent_disabled: bool,
}

impl From<&Connection> for SecurityRevision {
    fn from(connection: &Connection) -> Self {
        Self {
            login: connection.login.clone(),
            jump: connection.jump.clone(),
            incognito: connection.incognito,
            isolation: connection.isolation,
            startup: connection.startup,
            agent_disabled: connection.terminal.ai_agent_disabled,
        }
    }
}

impl Source {
    pub(in crate::workspace::terminal_agent) fn route_allowed(
        &self,
        policy: &ClipboardPolicy,
    ) -> bool {
        !self.security.incognito
            && self.security.isolation.is_none()
            && policy.minimum_isolation.is_none()
            && self.follow_up.as_ref().is_none_or(|scope| {
                policy.ai_chat_allowed
                    && scope
                        .redactor
                        .redaction_policy()
                        .equivalent_redactions(&policy.terminal_logging)
                    && scope
                        .vault_revision
                        .as_ref()
                        .is_none_or(|revision| revision.strong_count() != 0)
            })
            && policy
                .server_access()
                .first_blocked(self.reference.target.servers(self.reference.jump.as_ref()))
                .is_none()
    }

    pub(super) fn new(connection: &Connection, store_path: &Path) -> Self {
        Self {
            reference: ConnectionRef::from(connection),
            label: format!(
                "{} · {}",
                connection.name,
                connection.login.endpoint.label()
            ),
            directory: String::new(),
            group: None,
            security: SecurityRevision::from(connection),
            store_path: store_path.to_owned(),
            follow_up: None,
        }
    }

    pub(super) fn bind_follow_up(
        mut self,
        review: Arc<Review>,
        profile: ProfileChoice,
        profile_revision: [u8; 32],
        redactor: Redactor,
        vault_revision: Option<Weak<()>>,
    ) -> Self {
        self.directory = review.directory().to_owned();
        self.follow_up = Some(FollowUp {
            review,
            profile,
            profile_revision,
            redactor,
            vault_revision,
        });
        self
    }

    fn check_revision(&self, connection: &Connection) -> Result<(), &'static str> {
        if ConnectionRef::from(connection) != self.reference {
            return Err("The saved SSH login or jump route changed. Reopen AI Swarm to review it.");
        }
        if SecurityRevision::from(connection) != self.security {
            return Err("The saved SSH security settings changed. Reopen AI Swarm to review them.");
        }
        Ok(())
    }

    fn current_connection(&self) -> Result<Connection, &'static str> {
        let connection = Store::open(self.store_path.clone())
            .ok()
            .and_then(|store| store.get(self.reference.id).cloned())
            .ok_or("The reviewed saved SSH connection is no longer available.")?;
        self.check_revision(&connection)?;
        if !eligible(&connection) {
            return Err(
                "The saved connection requires a live terminal for its privacy, isolation or login settings.",
            );
        }
        if let Some(follow_up) = &self.follow_up {
            let current = crate::ai_manager::Configuration::load()
                .ok()
                .filter(crate::ai_manager::Configuration::enabled)
                .filter(|config| follow_up.profile.transport(config).is_ok())
                .and_then(|config| {
                    config.agent_profile_reference_revision(&follow_up.profile.reference)
                });
            if current != Some(follow_up.profile_revision) {
                return Err("The reviewed AI profile changed. Review the follow-up again.");
            }
            let trust = crate::ssh_connection::trust_store()
                .map_err(|_| "The SSH host-key store is unavailable.")?;
            follow_up
                .review
                .trust_snapshot(&self.reference, &trust)
                .map_err(|_| "A reviewed SSH host key changed. Review the follow-up again.")?;
        }
        Ok(connection)
    }

    pub(in crate::workspace::terminal_agent) fn revision_current(&self) -> bool {
        self.current_connection().is_ok()
    }
}

pub(super) fn catalog(id: window::Id, prior: Option<Vec<u64>>) -> Task<Desktop> {
    const MAX_SAVED_TARGETS: usize = 1024;
    snippet::work(
        "kortty-swarm-saved-targets",
        move || {
            let path = crate::profile::connections_path()
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            let store = Store::open(path.clone())
                .map_err(|_| "The saved SSH connections are unavailable.")?;
            let prior = prior.as_deref().unwrap_or_default();
            Ok(prior
                .iter()
                .filter_map(|&id| store.get(id))
                .filter(|connection| eligible(connection))
                .chain(
                    store.connections().iter().filter(|connection| {
                        !prior.contains(&connection.id) && eligible(connection)
                    }),
                )
                .take(MAX_SAVED_TARGETS)
                .map(|connection| {
                    let mut source = Source::new(connection, &path);
                    source.group = store.group_of(connection.id).map(str::to_owned);
                    source
                })
                .collect())
        },
        move |result| {
            Desktop::TerminalAgent(id, RunMessage::Swarm(GroupMessage::SavedTargets(result)))
        },
    )
}

pub(in crate::workspace::terminal_agent) fn preflight(
    id: window::Id,
    epoch: u64,
    source: Source,
) -> Task<Desktop> {
    snippet::work(
        "kortty-swarm-route-review",
        move || {
            let trust = crate::ssh_connection::trust_store()
                .map_err(|_| "Could not open the SSH host-key store.")?;
            Review::capture(source.reference, source.directory, &trust).map(Arc::new)
                .map_err(|_| "Verify the absolute remote directory and existing host keys for every SSH hop.")
        },
        move |result| Desktop::TerminalAgent(id, RunMessage::SavedReady(epoch, result)),
    )
}

impl Workspace {
    pub(super) fn new_saved_member(
        &self,
        source: Source,
        mode: Mode,
        purpose: Purpose,
    ) -> Result<Run, String> {
        if self.terminal_agents.runs.len() >= MAX_RUNS {
            return Err("Close an agent activity window before opening another.".into());
        }
        let uses_ai = matches!(purpose, Purpose::Agent(_));
        let current = self.saved_swarm_connection(&source, mode, uses_ai)?;
        // Retain only the sealed redactor. Approval resolves fresh credentials.
        let (_, redactor) = self.saved_swarm_credentials(&source, &current)?;
        let label = mask::prompt(&redactor, &source.label)?;
        let profiles = if let Purpose::Agent(profile) = &purpose {
            let config = self
                .ai_configuration()
                .ok_or("AI is unavailable under the current settings.")?;
            let profiles = swarm_profiles(&config);
            if !profiles.contains(profile) {
                return Err("The selected agent profile changed. Review it again.".into());
            }
            profiles
        } else {
            Vec::new()
        };
        Ok(Run::from_setup(RunSetup {
            binding: Binding::Saved(Box::new(source)),
            mode,
            label: label.clone(),
            manual_directory: None,
            context: format!("Selected saved SSH target {label}"),
            profiles,
            purpose,
            swarm: true,
            design: AgentDesign::Classic.layout(),
            redactor,
        }))
    }

    pub(in crate::workspace::terminal_agent) fn saved_swarm_connection(
        &self,
        source: &Source,
        mode: Mode,
        uses_ai: bool,
    ) -> Result<Connection, String> {
        self.saved_swarm_policy(source, mode, uses_ai)?;
        source.current_connection().map_err(str::to_owned)
    }

    pub(in crate::workspace::terminal_agent) fn saved_swarm_policy(
        &self,
        source: &Source,
        mode: Mode,
        uses_ai: bool,
    ) -> Result<(), &'static str> {
        let policy = self.swarm_policy_for(if uses_ai {
            ai_mode(mode)
        } else {
            SwarmMode::Snippet
        })?;
        if self.isolation_required() || !policy.minimum_isolation.is_none() {
            return Err("Managed policy requires an isolated live terminal for this target.");
        }
        if !source.route_allowed(&self.clipboard.policy) || !source.route_allowed(&policy) {
            return Err("Managed policy blocks the reviewed SSH target or jump host.");
        }
        if uses_ai
            && mode == Mode::Run
            && (source.security.agent_disabled
                || self.clipboard.policy.ai_agent_execution == AiAgentExecution::ReadOnly)
        {
            return Err("Agent command execution is disabled for this connection.");
        }
        Ok(())
    }

    pub(in crate::workspace::terminal_agent) fn saved_swarm_credentials(
        &self,
        source: &Source,
        current: &Connection,
    ) -> Result<(Credentials, Redactor), String> {
        let (credentials, redactor, _) =
            self.saved_swarm_credentials_with_revision(source, current)?;
        Ok((credentials, redactor))
    }

    fn saved_swarm_credentials_with_revision(
        &self,
        source: &Source,
        current: &Connection,
    ) -> Result<(Credentials, Redactor, Option<Weak<()>>), String> {
        source.check_revision(current)?;
        let secrets = Secrets::default();
        let mut vault_revision = None;
        let credentials = Credentials::resolve_directory_browser(
            &source.reference,
            current,
            self.clipboard.policy.server_access(),
            |id, kind, endpoint| {
                let vault = self
                    .vault
                    .as_ref()
                    .ok_or(crate::vault_window::VAULT_LOCKED)?;
                let value = vault.resolve(id, kind, endpoint)?;
                vault_revision = Some(
                    vault
                        .credential_revision()
                        .ok_or(crate::vault_window::VAULT_LOCKED)?,
                );
                secrets.add(value.as_str());
                Ok(value)
            },
        )?;
        let secrets = secrets
            .seal()
            .map_err(|_| "Could not verify the SSH credential mask.")?;
        Ok((
            credentials,
            Redactor::for_journal(secrets, self.clipboard.policy.terminal_logging.clone()),
            vault_revision,
        ))
    }

    pub(in crate::workspace::terminal_agent) fn saved_swarm_execution(
        &self,
        source: &Source,
        review: Arc<Review>,
        mode: Mode,
        uses_ai: bool,
    ) -> Result<(Execution, Redactor), String> {
        if review.reference() != &source.reference || review.directory() != source.directory {
            return Err("The saved SSH approval no longer matches its target.".into());
        }
        let current = self.saved_swarm_connection(source, mode, uses_ai)?;
        let (credentials, redactor) = self.saved_swarm_credentials(source, &current)?;
        let redactor = source
            .follow_up
            .as_ref()
            .map_or(redactor, |scope| scope.redactor.clone());
        Ok((
            Execution {
                review,
                current: ConnectionRef::from(&current),
                credentials,
                startup: current.startup,
            },
            redactor,
        ))
    }

    pub(in crate::workspace::terminal_agent) fn start_saved_swarm_member(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let source = run
            .binding
            .saved()
            .ok_or("The saved SSH route is unavailable.")?
            .clone();
        if self.terminal_agents.owns_saved(source.reference.id) {
            return Err("Another swarm member already owns this saved SSH connection.".into());
        }
        if run.mode == Mode::Plan {
            return Err("Planning requires an open terminal pane.".into());
        }
        self.saved_swarm_connection(&source, run.mode, run.profile().is_some())?;
        if let Some(follow_up) = source.follow_up {
            run.redactor = follow_up.redactor;
            if mask::prompt(&run.redactor, &run.goal)? != run.goal {
                return Err("The saved question no longer matches its reviewed mask.".into());
            }
            self.mask_https_agent_input(run)?;
            run.target = Some(super::super::RunTarget::Saved(follow_up.review));
            run.transport = Some(self.agent_transport(run)?);
            if run.mode == Mode::Ask {
                return self.agent_turn(id, run);
            }
            run.next(
                Phase::ProbeApproval,
                "Review the fixed environment probe before running it.",
            );
            return Ok(Task::none());
        }
        run.goal = mask::prompt(&run.redactor, &run.goal)?;
        self.mask_https_agent_input(run)?;
        if run.mode == Mode::Ask {
            return self.agent_turn(id, run);
        }
        if run.profile().is_some() {
            run.transport = Some(self.agent_transport(run)?);
        }
        let epoch = run.next(
            Phase::SavedPreflight,
            "Checking the saved SSH route and host keys…",
        );
        Ok(preflight(id, epoch, source))
    }
}

fn eligible(connection: &Connection) -> bool {
    !connection.incognito
        && connection.isolation.is_none()
        && connection.missing_username().is_none()
        && std::iter::once(&connection.login)
            .chain(connection.jump.iter())
            .all(|login| !login.keyboard_interactive && !login.auth.requires_interactive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::Configuration;
    use crate::connection_store::Auth;
    use crate::snippets::tests::Profile;
    use crate::ssh_dialog::LoginTarget;
    use crate::vault_window::{Message as VaultMessage, VaultWindow};
    use crate::workspace::terminal_agent::RunTarget;
    use kortty_vault::{CredentialInput, Secret, Vault};

    #[test]
    fn swarm_headless_resolves_fresh_credentials_and_blocks_changed_route_or_locked_vault() {
        let profile = Profile::new();
        let path = profile.0.join("vault.bin");
        let mut vault = Vault::create(&path, b"swarm-headless-test-master").unwrap();
        let password = "swarm-headless-fixture-password";
        let credential = vault
            .save(
                CredentialInput {
                    label: "Swarm fixture".into(),
                    username: "operator".into(),
                    server_pattern: "target.example.test".into(),
                    ..CredentialInput::default()
                },
                Secret::new(password.into()),
            )
            .unwrap();
        let mut connection = Connection {
            id: 0,
            name: "Saved target".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: LoginTarget {
                    host: "target.example.test".into(),
                    port: 22,
                    username: "operator".into(),
                },
                auth: Auth::VaultPassword(credential),
                keyboard_interactive: false,
            },
            jump: None,
            emulation: "xterm-256color".into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        };
        let root = profile.0.canonicalize().unwrap();
        let store_path = root.join("connections.json");
        let mut store = Store::open(store_path.clone()).unwrap();
        let saved = store.save(connection).unwrap();
        connection = store.get(saved).unwrap().clone();
        let mut source = Source::new(&connection, &store_path);
        source.directory = "/tmp".into();
        let mut window = VaultWindow::open(path).unwrap();
        window.restore_after_ssh_keys(vault);
        let mut workspace = Workspace {
            vault: Some(window),
            ..Workspace::default()
        };
        let config: Configuration = serde_json::from_value(serde_json::json!({"profiles": {
            "profiles":[{"id":1,"name":"Swarm fixture","endpoint":"127.0.0.1:12345","model":"test"}],
            "default":1,"last_id":1
        }})).unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let (credentials, redactor) = workspace
            .saved_swarm_credentials(&source, &connection)
            .unwrap();
        assert!(redactor.contains_known_secret(password));
        drop(credentials);
        let endpoint = kortty_ssh::Endpoint::new("target.example.test", 22).unwrap();
        let key = kortty_ssh::HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap();
        let trust = kortty_ssh::TrustStore::from_pins(vec![(endpoint, key)]).unwrap();
        let review = Arc::new(
            Review::capture(source.reference.clone(), source.directory.clone(), &trust).unwrap(),
        );
        workspace.clipboard.policy.ai_swarm_feature_allowed = false;
        let profile = swarm_profiles(&workspace.ai_configuration().unwrap()).remove(0);
        assert!(
            matches!(workspace.new_saved_member(source.clone(), Mode::Run, Purpose::Agent(profile)), Err(error) if error.contains("blocks AI Swarm"))
        );
        assert!(
            workspace
                .saved_swarm_connection(&source, Mode::Run, false)
                .is_ok()
        );
        workspace.clipboard.policy.ai_swarm_feature_allowed = true;
        let open = |workspace: &Workspace| {
            let profile = swarm_profiles(&workspace.ai_configuration().unwrap()).remove(0);
            let mut run = workspace
                .new_saved_member(source.clone(), Mode::Run, Purpose::Agent(profile))
                .unwrap();
            run.target = Some(RunTarget::Saved(Arc::clone(&review)));
            let epoch = run.next(Phase::ProbeApproval, "Review probe");
            (run, epoch)
        };
        let (mut changed, changed_epoch) = open(&workspace);
        let (mut locked, locked_epoch) = open(&workspace);
        let saved_agent = ModelReview::capture_with_trust(&workspace, &locked, trust).unwrap();
        let profile = swarm_profiles(&workspace.ai_configuration().unwrap()).remove(0);
        let targetless = workspace
            .new_saved_member(source.clone(), Mode::Ask, Purpose::Agent(profile))
            .unwrap();
        assert!(targetless.target.is_none());
        let unpinned = kortty_ssh::TrustStore::from_pins(Vec::new()).unwrap();
        let advisory = ModelReview::capture_with_trust(&workspace, &targetless, unpinned).unwrap();
        assert!(advisory.pins().is_empty());
        assert!(advisory.require_current(&workspace, &targetless).is_ok());
        assert!(saved_agent.require_current(&workspace, &locked).is_ok());

        connection.terminal.ai_skills.push(9);
        store.save(connection.clone()).unwrap();
        assert!(
            advisory
                .check_worker()
                .unwrap_err()
                .contains("skill assignments changed")
        );
        assert!(
            saved_agent
                .check_worker()
                .unwrap_err()
                .contains("skill assignments changed")
        );
        connection.terminal.ai_skills.clear();
        connection.login.endpoint.port = 2222;
        store.save(connection.clone()).unwrap();
        assert!(
            advisory
                .check_worker()
                .unwrap_err()
                .contains("route changed")
        );
        let task = workspace.agent_action(
            window::Id::unique(),
            &mut changed,
            RunMessage::Approve(changed_epoch),
        );
        assert!(iced_runtime::task::into_stream(task).is_none());
        assert!(changed.phase == Phase::Blocked && changed.notice.contains("route changed"));
        connection.login.endpoint.port = 22;
        store.save(connection.clone()).unwrap();
        let auth = std::mem::replace(&mut connection.login.auth, Auth::Prompt);
        assert!(
            matches!(workspace.saved_swarm_credentials(&source, &connection), Err(error) if error.contains("security settings changed"))
        );
        connection.login.auth = auth;
        let _ = workspace.vault.as_mut().unwrap().update(VaultMessage::Lock);
        assert!(
            advisory
                .check_worker()
                .unwrap_err()
                .contains("vault changed or was locked")
        );
        assert!(
            saved_agent
                .check_worker()
                .unwrap_err()
                .contains("vault changed or was locked")
        );
        let task = workspace.agent_action(
            window::Id::unique(),
            &mut locked,
            RunMessage::Approve(locked_epoch),
        );
        assert!(iced_runtime::task::into_stream(task).is_none());
        assert!(
            locked.phase == Phase::Blocked && locked.notice == crate::vault_window::VAULT_LOCKED
        );
    }
}
