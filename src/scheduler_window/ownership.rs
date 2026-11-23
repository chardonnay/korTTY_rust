use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task};
use kortty::ssh_runtime::account_lookup::{INPUT_LIMIT, Kind, valid_name};
use kortty_ssh::{ConnectOptions, Endpoint, ExecConnection, HostKey, TrustStore};

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::{Connection, SavedLogin, Store};
use crate::job_executor::Credentials;
use crate::sftp_window::worker::resolve_scheduler_account;
use crate::{profile, ssh_connection};

#[cfg(test)]
use super::Field;
use super::sync;
use super::{ConnectionRef, Message as Scheduler, Page, SchedulerView, Source, SudoMode};

const CHANGED: &str =
    "The saved SSH route or its host keys changed. Review the account again or enter a numeric ID.";
pub(crate) const DENIED: &str = "Application policy forbids account lookup for this job.";

#[derive(Clone)]
pub enum Message {
    Start(Kind),
    Name(u64, String),
    Lookup(u64),
    Resolved(u64, Result<u32, String>),
    Use(u64),
    Cancel(u64),
}

pub(super) struct Route {
    reference: ConnectionRef,
    local_logins: Option<(SavedLogin, Option<SavedLogin>)>,
    pins: Vec<(Endpoint, HostKey)>,
}

impl Route {
    fn capture(reference: ConnectionRef, local: Option<&Connection>) -> Result<Self, String> {
        let local_logins = if reference.shared_identity().is_some() {
            reference.require_installed_shared_route()?;
            None
        } else {
            let local = local
                .filter(|local| ConnectionRef::from(*local) == reference)
                .ok_or(CHANGED)?;
            if local.incognito || local.requires_isolated_launch() {
                return Err(
                    "This saved connection requires privacy or isolation unavailable to jobs."
                        .into(),
                );
            }
            Some((local.login.clone(), local.jump.clone()))
        };
        let trust = ssh_connection::trust_store()
            .map_err(|_| "Could not access the SSH host-key trust store.")?;
        let trust = reference.strict_trust(trust)?;
        let pins = sync::pins(&reference, &trust).map_err(|error| error.to_string())?;
        Ok(Self {
            reference,
            local_logins,
            pins,
        })
    }

    pub(super) fn matches(&self, reference: &ConnectionRef) -> bool {
        &self.reference == reference
    }

    pub(super) fn authorize(&self, sudo: SudoMode) -> Result<(), &'static str> {
        let policy = ClipboardPolicy::load();
        self.check_policy(&policy, sudo)?;
        if let Some((login, jump)) = &self.local_logins {
            let store = profile::connections_path()
                .and_then(Store::open)
                .map_err(|_| CHANGED)?;
            let saved = store.get(self.reference.id).ok_or(CHANGED)?;
            if &saved.login != login || &saved.jump != jump {
                return Err(CHANGED);
            }
            if saved.incognito || store.connection_requires_isolation(saved) {
                return Err(
                    "This saved connection requires privacy or isolation unavailable to jobs.",
                );
            }
        } else {
            self.reference
                .require_installed_shared_route()
                .map_err(|_| CHANGED)?;
        }
        let trust = ssh_connection::trust_store().map_err(|_| CHANGED)?;
        self.check_pins(&trust)
    }

    fn check_policy(&self, policy: &ClipboardPolicy, sudo: SudoMode) -> Result<(), &'static str> {
        let access = policy.server_access();
        if policy.problem.is_some()
            || !policy.minimum_isolation.is_none()
            || (sudo != SudoMode::Disabled && !policy.scheduler_sudo_allowed)
            || access
                .first_blocked(
                    self.reference
                        .jump
                        .iter()
                        .chain(std::iter::once(&self.reference.target))
                        .map(|login| (login.host.as_str(), login.port)),
                )
                .is_some()
            || self
                .pins
                .iter()
                .any(|(endpoint, _)| !access.allows(endpoint.wire_host().as_ref(), endpoint.port()))
        {
            return Err(DENIED);
        }
        Ok(())
    }

    fn check_pins(&self, trust: &TrustStore) -> Result<(), &'static str> {
        if sync::pins(&self.reference, trust).is_ok_and(|pins| pins == self.pins) {
            Ok(())
        } else {
            Err(CHANGED)
        }
    }
}

pub(super) struct Review {
    route: Arc<Route>,
    kind: Kind,
    source: Source,
    sudo: SudoMode,
    generation: u64,
    name: String,
    cancelled: Arc<AtomicBool>,
    result: Option<Result<u32, String>>,
    busy: bool,
}

impl Review {
    fn new(route: Route, kind: Kind, source: Source, sudo: SudoMode) -> Self {
        Self {
            route: Arc::new(route),
            kind,
            source,
            sudo,
            generation: generation(),
            name: String::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            result: None,
            busy: false,
        }
    }

    fn can_lookup(&self) -> bool {
        !self.busy && self.id().is_none() && valid_name(&self.name)
    }

    fn id(&self) -> Option<u32> {
        self.result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .copied()
    }

    fn lookup(&mut self, credentials: Credentials) -> Task<Scheduler> {
        self.busy = true;
        self.result = None;
        self.generation = generation();
        let generation = self.generation;
        let name = self.name.clone();
        let kind = self.kind;
        let sudo = self.sudo;
        let route = Arc::clone(&self.route);
        let cancelled = Arc::clone(&self.cancelled);
        let (report, result) = async_channel::bounded(1);
        let started = thread::Builder::new()
            .name("kortty-job-account-lookup".into())
            .spawn(move || {
                let lookup = resolve(route, credentials, sudo, kind, &name, cancelled);
                let _ = report.try_send(lookup);
            });
        Task::future(async move {
            let resolved = match started {
                Ok(_) => result.recv().await.unwrap_or_else(|_| {
                    Err("The server account lookup ended without a result.".into())
                }),
                Err(_) => Err("Could not start the server account lookup.".into()),
            };
            Scheduler::Ownership(Message::Resolved(generation, resolved))
        })
    }

    pub(super) fn view(&self) -> Element<'_, Scheduler> {
        let generation = self.generation;
        let (heading, placeholder, id_label) = match self.kind {
            Kind::User => ("Resolve Owner by Name", "Server user name", "UID"),
            Kind::Group => ("Resolve Group by Name", "Server group name", "GID"),
        };
        let field = text_input(placeholder, &self.name);
        let field = if self.busy {
            field
        } else {
            field
                .on_input(move |name| Scheduler::Ownership(Message::Name(generation, name)))
                .on_submit_maybe(
                    self.can_lookup()
                        .then_some(Scheduler::Ownership(Message::Lookup(generation))),
                )
        };
        let mut content = column![
            text(heading).size(20),
            text(self.route.reference.target.label())
        ];
        if let Some(jump) = &self.route.reference.jump {
            content = content.push(text(format!("Jump host · {}", jump.label())));
        }
        for fingerprint in sync::fingerprints(&self.route.pins) {
            content = content.push(text(fingerprint).size(13));
        }
        content
            .push(
                row![
                    field,
                    button("Look Up").on_press_maybe(
                        self.can_lookup()
                            .then_some(Scheduler::Ownership(Message::Lookup(generation)))
                    ),
                ]
                .spacing(8),
            )
            .push(
                self.busy
                    .then(|| text("Looking up the account on the saved SSH route…")),
            )
            .push(self.result.as_ref().map(|result| match result {
                Ok(id) => text(format!("{} → {id_label} {id}", self.name)),
                Err(error) => text(error),
            }))
            .push(
                row![
                    button(match self.kind {
                        Kind::User => "Use This UID",
                        Kind::Group => "Use This GID",
                    })
                    .on_press_maybe(
                        self.id()
                            .map(|_| Scheduler::Ownership(Message::Use(generation)))
                    ),
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(Scheduler::Ownership(Message::Cancel(generation))),
                ]
                .spacing(8),
            )
            .padding(12)
            .spacing(8)
            .into()
    }
}

impl Drop for Review {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl SchedulerView {
    pub(crate) fn ownership_target(&self) -> Option<(&ConnectionRef, SudoMode)> {
        if self.page != Page::Action || self.confirm.is_some() || self.hosted_close.is_some() {
            return None;
        }
        self.draft
            .ownership_target()
            .map(|target| (target, self.draft.sudo))
    }

    pub(crate) fn reviewing_ownership(&self) -> bool {
        self.ownership_review.is_some()
    }

    pub(crate) fn cancel_ownership_review(&mut self) {
        self.ownership_review = None;
    }

    pub(crate) fn begin_ownership_review(
        &mut self,
        kind: Kind,
        connection: Option<&Connection>,
    ) -> Result<(), String> {
        let (reference, sudo) = self
            .ownership_target()
            .ok_or("Choose one saved SSH connection.")?;
        let route = Route::capture(reference.clone(), connection)?;
        self.ownership_review = Some(Review::new(route, kind, self.draft.source, sudo));
        Ok(())
    }

    pub(crate) fn ownership_lookup_target(&self, generation: u64) -> Option<&ConnectionRef> {
        let review = self.ownership_review.as_ref()?;
        let (current, sudo) = self.ownership_target()?;
        (review.generation == generation
            && review.can_lookup()
            && review.route.matches(current)
            && review.source == self.draft.source
            && review.sudo == sudo)
            .then_some(current)
    }

    pub(crate) fn lookup_ownership(
        &mut self,
        generation: u64,
        credentials: Result<Credentials, &'static str>,
    ) -> Task<Scheduler> {
        if self.ownership_lookup_target(generation).is_none() {
            return Task::none();
        }
        let Some(review) = self.ownership_review.as_mut() else {
            return Task::none();
        };
        match credentials {
            Ok(credentials) => review.lookup(credentials),
            Err(error) => {
                review.result = Some(Err(error.into()));
                Task::none()
            }
        }
    }

    pub(crate) fn update_ownership(&mut self, message: Message) {
        self.update_ownership_with(message, Route::authorize);
    }

    fn update_ownership_with(
        &mut self,
        message: Message,
        authorize: impl Fn(&Route, SudoMode) -> Result<(), &'static str>,
    ) {
        let generation = match &message {
            Message::Start(_) | Message::Lookup(_) => return,
            Message::Name(generation, _)
            | Message::Resolved(generation, _)
            | Message::Use(generation)
            | Message::Cancel(generation) => *generation,
        };
        let Some(review) = self
            .ownership_review
            .as_ref()
            .filter(|r| r.generation == generation)
        else {
            return;
        };
        if matches!(message, Message::Cancel(_)) {
            self.ownership_review = None;
            return;
        }
        if !self.ownership_target().is_some_and(|(current, sudo)| {
            review.route.matches(current)
                && review.source == self.draft.source
                && review.sudo == sudo
        }) {
            self.ownership_review = None;
            self.report_status(CHANGED.into());
            return;
        }
        let Some(review) = self.ownership_review.as_mut() else {
            return;
        };
        match message {
            Message::Name(_, name) if !review.busy && name.len() <= INPUT_LIMIT => {
                if review.name != name {
                    review.name = name;
                    review.result = None;
                }
            }
            Message::Resolved(_, result) if review.busy => {
                review.busy = false;
                review.result = Some(result.and_then(|id| {
                    authorize(&review.route, review.sudo)?;
                    Ok(id)
                }));
            }
            Message::Use(_) => {
                let Some(id) = review.id() else {
                    return;
                };
                if let Err(error) = authorize(&review.route, review.sudo) {
                    review.result = Some(Err(error.into()));
                    return;
                }
                let route = Arc::clone(&review.route);
                self.draft.apply_ownership_id(review.kind, id, route);
                self.dirty = true;
                self.privileged_review = Default::default();
                self.ownership_review = None;
            }
            _ => {}
        }
    }
}

fn resolve(
    route: Arc<Route>,
    credentials: Credentials,
    sudo: SudoMode,
    kind: Kind,
    name: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<u32, String> {
    let pinned = TrustStore::from_pins(route.pins.clone()).map_err(|_| CHANGED)?;
    let cancelled_exec = Arc::clone(&cancelled);
    let stop = move || cancelled.load(Ordering::Acquire);
    let authorize = move || {
        if cancelled_exec.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Account lookup cancelled.",
            ));
        }
        route.authorize(sudo).map_err(io::Error::other)
    };
    authorize().map_err(|error| error.to_string())?;
    let (target, jumps) = credentials.into_swarm_route()?;
    let first = &jumps.first().unwrap_or(&target).endpoint;
    let options = ConnectOptions::QUICK;
    let tcp = ssh_connection::connect_transport_with_options(first, options, &stop)
        .map_err(|_| "Could not connect to the saved SSH route.")?;
    let connection = ExecConnection::connect_preconnected(
        tcp,
        (target, jumps),
        pinned,
        options,
        &stop,
        &authorize,
    )
    .map_err(|_| "Could not authenticate the saved SSH route with its pinned host keys.")?;
    let opener = connection
        .exec_opener()
        .ok_or("The saved SSH route has no command channel.")?;
    resolve_scheduler_account(opener, kind, name, stop, Arc::new(authorize))
        .map_err(|error| error.message)
}

fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_store::Auth;
    use crate::scheduler_window::{ConnectionChoice, RemoteAction, Source};
    use crate::ssh_dialog::LoginTarget;
    use kortty_jobs::JobId;

    fn saved_login(host: &str) -> SavedLogin {
        SavedLogin {
            endpoint: LoginTarget {
                host: host.into(),
                port: 22,
                username: "operator".into(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
        }
    }

    fn key() -> HostKey {
        HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap()
    }

    fn route() -> Route {
        let login = saved_login("target.example");
        let jump = Some(saved_login("jump.example"));
        let reference = ConnectionRef {
            teamwork: None,
            id: 1,
            target: login.endpoint.clone(),
            jump: jump.as_ref().map(|login| login.endpoint.clone()),
        };
        let pins = ["jump.example", "target.example"]
            .into_iter()
            .map(|host| (Endpoint::new(host, 22).unwrap(), key()))
            .collect();
        Route {
            reference,
            local_logins: Some((login, jump)),
            pins,
        }
    }

    #[test]
    fn scheduler_account_lookup_refuses_a_missing_or_replaced_jump_pin() {
        let route = route();
        let pinned = TrustStore::from_pins(route.pins.clone()).unwrap();
        assert!(route.check_pins(&pinned).is_ok());
        let missing_jump = TrustStore::from_pins(route.pins[1..].to_vec()).unwrap();
        assert_eq!(route.check_pins(&missing_jump), Err(CHANGED));
        let mut rotated = route.pins.clone();
        rotated[0].1 = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbi",
        )
        .unwrap();
        assert_eq!(
            route.check_pins(&TrustStore::from_pins(rotated).unwrap()),
            Err(CHANGED),
        );
    }

    #[test]
    fn scheduler_account_lookup_respects_sudo_authority_and_required_isolation() {
        use crate::session_security::IsolationLevel;

        let route = route();
        let mut policy = ClipboardPolicy {
            scheduler_sudo_allowed: false,
            ..ClipboardPolicy::default()
        };
        assert_eq!(route.check_policy(&policy, SudoMode::Disabled), Ok(()));
        assert_eq!(
            route.check_policy(&policy, SudoMode::Passwordless),
            Err(DENIED)
        );
        policy.minimum_isolation = IsolationLevel::Process;
        assert_eq!(route.check_policy(&policy, SudoMode::Disabled), Err(DENIED));
    }

    #[test]
    fn scheduler_account_review_ignores_stale_results_and_requires_explicit_acceptance() {
        let route = route();
        let choice = ConnectionChoice::Saved {
            reference: route.reference.clone(),
            name: "Fixture".into(),
        };
        let mut view = SchedulerView {
            page: Page::Action,
            ..Default::default()
        };
        view.draft.source = Source::RemoteChown;
        assert!(view.ownership_target().is_none());
        view.draft.select_connection(choice);
        view.draft.set(Field::Name, "Review fixture".into());
        view.draft.set(Field::RemotePath, "/tmp/fixture".into());
        view.draft.set(Field::RemoteGid, "42".into());
        let mut review = Review::new(route, Kind::Group, Source::RemoteChown, SudoMode::Disabled);
        let generation = review.generation;
        review.name = "staff".into();
        review.busy = true;
        view.ownership_review = Some(review);
        let id = JobId::new("account-review-fixture").unwrap();

        view.update_ownership_with(Message::Resolved(generation + 1, Ok(123)), |_, _| {
            panic!("A stale result must be ignored")
        });
        assert!(view.ownership_review.as_ref().unwrap().busy);
        view.update_ownership_with(Message::Resolved(generation, Ok(100)), |_, _| Ok(()));
        let original = view.draft.build(id.clone()).unwrap();
        assert!(matches!(
            original.action().remote_action().unwrap(),
            RemoteAction::Chown {
                uid: None,
                gid: Some(42),
                ..
            }
        ));
        assert!(!view.dirty);

        view.update_ownership_with(Message::Use(generation), |_, _| Ok(()));
        assert!(view.dirty);
        assert!(view.ownership_review.is_none());
        let chosen = view
            .draft
            .build_with_ownership_check(id.clone(), |_, _| Ok(()))
            .unwrap();
        assert!(matches!(
            chosen.action().remote_action().unwrap(),
            RemoteAction::Chown {
                uid: None,
                gid: Some(100),
                ..
            }
        ));
        let saved_action = serde_json::to_string(chosen.action()).unwrap();
        assert!(!saved_action.contains("staff"));
        assert!(!saved_action.contains("fingerprint"));

        let mut other = self::route().reference;
        other.id = 2;
        let other = ConnectionChoice::Saved {
            reference: other,
            name: "Other".into(),
        };
        assert!(view.draft.add_connection(other.clone()).unwrap());
        assert!(
            matches!(view.draft.build(id.clone()), Err(error) if error.contains("selected route"))
        );
        view.draft.select_connection(other);
        assert!(
            matches!(view.draft.build(id.clone()), Err(error) if error.contains("selected route"))
        );
        view.draft.set(Field::RemoteGid, "101".into());
        let edited = view.draft.build(id).unwrap();
        assert!(matches!(
            edited.action().remote_action().unwrap(),
            RemoteAction::Chown {
                uid: None,
                gid: Some(101),
                ..
            }
        ));
        assert!(
            !serde_json::to_string(edited.action())
                .unwrap()
                .contains("staff")
        );
    }
}
