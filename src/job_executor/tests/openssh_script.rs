use std::fs;
use std::io::{ErrorKind, Write as _};
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use kortty_jobs::{JobId, RunRequest, RunTrigger, Scheduler};
use kortty_ssh::{Endpoint, HostKey, HostKeyStatus, TrustMode, TrustStore};
use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};
use serde_json::json;

use crate::clipboard_policy::ServerAccess;
use crate::connection_store::{Auth, Connection, SavedLogin, Store};
use crate::scheduler_window::{ConnectionRef, Message, SchedulerWindow};
use crate::snippets::{Input, Library, Variable};
use crate::ssh_dialog::LoginTarget;
use crate::ssh_key_catalog::{Catalog, Edit, KeyDraft, KeyInput};
use crate::vault_window::{Kind, resolve_credential};

use super::{
    BorrowedArchivePassword, Completion, Credentials, ExecutionPolicy, RunOutcome, SshCommand,
    batch, run, run_command, scheduled_request, thread, virtual_terminal,
};

#[cfg(target_os = "linux")]
mod privileged_archive;
mod scheduled_swarm;

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-job-script-{:016x}",
            u64::from_ne_bytes(nonce)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn keygen(path: &Path) {
    let output = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Sshd {
    profile: Profile,
    user_key: PathBuf,
    user: String,
    config: PathBuf,
    listener: TcpListener,
    trust: TrustStore,
    port: u16,
}

impl Sshd {
    fn new() -> Self {
        let profile = Profile::new();
        let host = profile.0.join("host");
        let user_key = profile.0.join("user");
        keygen(&host);
        keygen(&user_key);
        let user = Command::new("id").arg("-un").output().unwrap();
        assert!(user.status.success());
        let user = String::from_utf8(user.stdout).unwrap();
        let user = user.trim().to_owned();
        let config = profile.0.join("sshd.conf");
        let quote_path = |path: &Path| {
            format!(
                "\"{}\"",
                path.to_str()
                    .unwrap()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            )
        };
        fs::write(
            &config,
            format!(
                "HostKey {}\nAuthorizedKeysFile {}\nAllowUsers {user}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
             UsePAM no\nStrictModes yes\nLogLevel DEBUG2\nPrintMotd no\nPrintLastLog no\nPermitTTY yes\nSubsystem sftp internal-sftp\n",
                quote_path(&host),
                quote_path(&user_key.with_extension("pub")),
            ),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let trust = TrustStore::new(profile.0.join("known_hosts"), TrustMode::Strict);
        let public = fs::read_to_string(host.with_extension("pub")).unwrap();
        assert_eq!(
            trust
                .remember(
                    &Endpoint::new("127.0.0.1", port).unwrap(),
                    &HostKey::from_openssh(public.trim()).unwrap()
                )
                .unwrap(),
            HostKeyStatus::Known
        );
        Self {
            profile,
            user_key,
            user,
            config,
            listener,
            trust,
            port,
        }
    }

    fn spawn(&self) -> JoinHandle<Output> {
        let server = self.listener.try_clone().unwrap();
        let config = self.config.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let stream = loop {
                match server.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("No scheduled SSH connection: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let input: OwnedFd = stream.into();
            let output = input.try_clone().unwrap();
            Command::new("/usr/sbin/sshd")
                .args(["-ie", "-f"])
                .arg(config)
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(output))
                .stderr(Stdio::piped())
                .output()
                .unwrap()
        })
    }

    fn pin(&self) -> (Endpoint, HostKey) {
        let key = fs::read_to_string(self.profile.0.join("host.pub")).unwrap();
        (
            Endpoint::new("127.0.0.1", self.port).unwrap(),
            HostKey::from_openssh(key.trim()).unwrap(),
        )
    }

    fn saved_login(&self, passphrase: Option<String>) -> SavedLogin {
        SavedLogin {
            endpoint: LoginTarget {
                host: "127.0.0.1".into(),
                port: self.port,
                username: self.user.clone(),
            },
            auth: Auth::KeyFile {
                path: self.user_key.clone(),
                passphrase,
            },
            keyboard_interactive: false,
        }
    }

    fn run_sftp(
        &self,
        remote: serde_json::Value,
        trust: TrustStore,
        file_transfer_allowed: bool,
    ) -> (Completion, String) {
        self.run_sftp_with_password(remote, trust, file_transfer_allowed, Ok(None))
    }

    fn run_sftp_with_password(
        &self,
        remote: serde_json::Value,
        trust: TrustStore,
        file_transfer_allowed: bool,
        password: BorrowedArchivePassword<'_>,
    ) -> (Completion, String) {
        let daemon = self.spawn();
        let (_, mut request) = scheduled_request();
        request.action = Arc::new(
            serde_json::from_value(json!({
                "host": "127.0.0.1", "port": self.port, "username": self.user,
                "key_file": self.user_key, "remote": remote,
            }))
            .unwrap(),
        );
        let completion = run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(file_transfer_allowed),
            Ok(None),
            password,
            || Ok(trust),
        );
        let output = daemon.join().unwrap();
        let log = String::from_utf8(output.stderr).unwrap();
        assert!(completion.recording.is_none());
        (completion, log)
    }
}

fn scheduled_batch(
    root: &Path,
    servers: &[&Sshd],
    body: &str,
    arguments: Vec<String>,
) -> (Scheduler<SshCommand>, RunRequest<SshCommand>, Store) {
    let path = root.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let mut references = Vec::new();
    for (index, server) in servers.iter().enumerate() {
        let id = store
            .save(Connection {
                id: 0,
                name: format!("Test target {}", index + 1),
                tag: String::new(),
                login: server.saved_login(None),
                jump: None,
                emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
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
            })
            .unwrap();
        references.push(ConnectionRef::from(store.get(id).unwrap()));
    }
    let (scheduler, mut request) = scheduled_request();
    request.action = Arc::new(
        serde_json::from_value(json!({
            "connection": references[0], "additional_connections": references[1..],
            "script": {"language":"bash", "body": body, "arguments": arguments},
        }))
        .unwrap(),
    );
    (scheduler, request, Store::open(path).unwrap())
}

#[cfg(target_os = "linux")]
fn privileged_fixture_route() -> (Connection, Vec<(Endpoint, HostKey)>) {
    use std::net::SocketAddr;

    let required = |key| std::env::var(key).unwrap_or_else(|_| panic!("set {key} for the fixture"));
    let address: SocketAddr = required("KORTTY_SSH_EXEC_ADDRESS").parse().unwrap();
    assert!(address.ip().is_loopback());
    let host = address.ip().to_string();
    let account = Connection {
        id: 1,
        name: "Privileged transfer fixture".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: host.clone(),
                port: address.port(),
                username: required("KORTTY_SSH_EXEC_USER"),
            },
            auth: Auth::KeyFile {
                path: PathBuf::from(required("KORTTY_SSH_EXEC_PRIVATE_KEY")),
                passphrase: None,
            },
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
    let pins = vec![(
        Endpoint::new(&host, address.port()).unwrap(),
        HostKey::from_openssh(&required("KORTTY_SSH_EXEC_HOST_KEY")).unwrap(),
    )];
    (account, pins)
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a dedicated OpenSSH fixture with private Linux mount namespace and mock sudo"]
fn openssh_scheduled_reviewed_privileged_tree_runs_once_on_its_pinned_route() {
    scheduled_privileged_tree(false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a dedicated OpenSSH fixture with a protected source tree and mock sudo"]
fn openssh_scheduled_reviewed_privileged_tree_download_runs_once_on_its_pinned_route() {
    scheduled_privileged_tree(true);
}

#[cfg(target_os = "linux")]
fn scheduled_privileged_tree(download: bool) {
    use crate::job_executor::JobAuth;
    use crate::scheduler_window::{Message, Page, SchedulerWindow};
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

    let parent = PathBuf::from(
        std::env::var_os("KORTTY_SSH_PRIVILEGED_TREE_HOST_PARENT")
            .expect("supply the private host directory mounted as /fixtures"),
    );
    let log = PathBuf::from(
        std::env::var_os("KORTTY_SSH_EXEC_LOG").expect("supply the dedicated fixture sshd log"),
    );
    let auth_count = || {
        fs::read_to_string(&log)
            .unwrap()
            .matches("Accepted publickey")
            .count()
    };
    let (account, pins) = privileged_fixture_route();
    let reference = ConnectionRef::from(&account);
    let trust = TrustStore::from_pins(pins).unwrap();
    let profile = Profile::new();
    let local = profile.0.join("local");
    let body: Vec<_> = (0..196_610).map(|index| (index % 251) as u8).collect();
    let (target, kind, direction, remote, excludes) = if download {
        let source = parent.join("download-tree");
        assert_eq!(fs::read(source.join("nested/data.bin")).unwrap(), body);
        assert!(source.join("nested/empty").is_dir());
        (
            local.clone(),
            "privileged_tree_download",
            "download",
            "/fixtures/download-tree",
            "",
        )
    } else {
        fs::create_dir(&local).unwrap();
        fs::create_dir(local.join("nested")).unwrap();
        fs::create_dir(local.join("nested/empty")).unwrap();
        fs::write(local.join("excluded.tmp"), b"outside the review").unwrap();
        fs::write(local.join("nested/data.bin"), &body).unwrap();
        (
            parent.join("scheduled-tree"),
            "privileged_tree_upload",
            "upload",
            "/fixtures/scheduled-tree",
            "*.tmp",
        )
    };
    assert!(!target.exists(), "fixture destination must start absent");
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference, "sudo": "vault",
        "remote": {"kind": kind, "direction": direction,
                   "local": local, "remote": remote, "excludes": excludes}
    }))
    .unwrap();
    let id = JobId::new("reviewed-privileged-tree").unwrap();
    let path = profile.0.join("scheduler.json");
    let credentials = |action: &SshCommand| {
        let saved = Credentials::resolve(action, &account, &ServerAccess::default(), |_, _, _| {
            Err("unexpected SSH key credential")
        })
        .map(Box::new);
        let JobAuth::Saved(saved) = JobAuth::Saved(saved)
            .with_sudo_passwords(|_| Ok(Secret::new("kortty-file-fixture".to_owned())))
        else {
            unreachable!();
        };
        *saved.unwrap()
    };
    let initial_credentials = credentials(&action);
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id, "Reviewed privileged tree", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    {
        let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
        runtime.set_execution_allowed(true);
        let mut view = runtime.new_view();
        runtime.update_view(&mut view, Message::Page(Page::Action));
        let request = runtime.begin_sync_review(&mut view).unwrap();
        let ticket = request.ticket.clone();
        let read_trust = trust.clone();
        let before = auth_count();
        let preview = super::super::remote::sync::preview_with_trust(
            request,
            initial_credentials,
            move || Ok(read_trust),
        )
        .recv_blocking()
        .unwrap()
        .expect("actual privileged tree review");
        assert_eq!(auth_count(), before + 1);
        assert!(!target.exists());
        assert_eq!(
            preview.preview.paths,
            vec![
                (true, "nested".into()),
                (false, "nested/data.bin".into()),
                (true, "nested/empty".into())
            ]
        );
        runtime.update_view(
            &mut view,
            Message::SyncPreviewed(ticket, Ok(Arc::new(preview))),
        );
        runtime.update_view(&mut view, Message::ApproveSync);
        runtime.update_view(&mut view, Message::Save);
    }
    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    let request = runtime
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let policy = ExecutionPolicy {
        sync: Some(runtime.sync_run_context(&request).unwrap()),
        ..ExecutionPolicy::unrestricted(true)
    };
    let execute = || {
        run(
            &request,
            None,
            None,
            &policy,
            Ok(Some(credentials(&request.action))),
            Ok(None),
            || Ok(trust.clone()),
        )
    };
    let before = auth_count();
    let outcome = execute();
    assert_eq!(outcome.outcome, RunOutcome::Success, "{:?}", outcome.detail);
    assert_eq!(
        auth_count(),
        before + 1,
        "run must reuse one authenticated route"
    );
    assert_eq!(
        target.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(target.join("nested/data.bin")).unwrap(), body);
    assert!(target.join("nested/empty").is_dir());
    assert!(!target.join("excluded.tmp").exists());
    assert_eq!(execute().outcome, RunOutcome::Blocked);
    assert_eq!(
        auth_count(),
        before + 1,
        "consumed receipt must block before dial"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires an explicit OpenSSH fixture with private Linux mount namespace and mock sudo"]
fn openssh_scheduled_privileged_binary_transfer_reuses_pinned_route_and_never_replaces() {
    use crate::job_executor::JobAuth;
    use crate::scheduler_window::{RemoteAction, privileged::approve_fixture};

    let remote = std::env::var("KORTTY_SSH_PRIVILEGED_TRANSFER_PATH")
        .expect("set KORTTY_SSH_PRIVILEGED_TRANSFER_PATH for the fixture");
    assert_eq!(remote, "/fixtures/scheduled.bin");
    let profile = Profile::new();
    let (account, pins) = privileged_fixture_route();
    let reference = ConnectionRef::from(&account);
    let trust = TrustStore::from_pins(pins.clone()).unwrap();
    let (_, mut request) = scheduled_request();
    let source = profile.0.join("upload");
    let destination = profile.0.join("download");
    let bytes: Vec<_> = (0..196_610).map(|index| (index % 256) as u8).collect();
    fs::write(&source, &bytes).unwrap();
    let action = |direction, local: &Path| -> SshCommand {
        serde_json::from_value::<SshCommand>(json!({
            "connection": reference,
            "sudo": if direction == "upload" { "vault" } else { "passwordless" },
            "remote": {"kind": "privileged", "direction": direction, "local": local, "remote": remote}
        }))
        .unwrap()
        .validated()
        .unwrap()
    };
    let credentials = |action: &SshCommand, password: Option<&str>| -> super::super::SavedAuth {
        let credentials =
            Credentials::resolve(action, &account, &ServerAccess::default(), |_, _, _| {
                panic!("The plain fixture key must not read the vault")
            })
            .unwrap();
        let Some(password) = password else {
            return Ok(Some(credentials));
        };
        let JobAuth::Saved(resolved) = JobAuth::Saved(Ok(Box::new(credentials)))
            .with_sudo_passwords(|_| Ok(Secret::new(password.to_owned())))
        else {
            unreachable!();
        };
        resolved.map(|value| Some(*value))
    };
    let exec = |request: &RunRequest<SshCommand>, policy: &ExecutionPolicy, password| {
        run(
            request,
            None,
            None,
            policy,
            credentials(&request.action, password),
            Ok(None),
            || Ok(trust.clone()),
        )
    };
    let good_password = "kortty-file-fixture";
    request.action = Arc::new(action("upload", &source));
    let mut policy = ExecutionPolicy::unrestricted(true);
    let blocked = run(
        &request,
        None,
        None,
        &policy,
        credentials(&request.action, Some(good_password)),
        Ok(None),
        || panic!("Missing approval must block before looking up host keys"),
    );
    assert_eq!(blocked.outcome, RunOutcome::Blocked);

    policy.privileged = Some(
        approve_fixture(
            &profile.0,
            &request.job,
            Arc::make_mut(&mut request.action),
            &pins,
        )
        .unwrap(),
    );
    let no_vault = run(
        &request,
        None,
        None,
        &policy,
        credentials(&request.action, None),
        Ok(None),
        || panic!("Missing sudo credentials must block before looking up host keys"),
    );
    assert_eq!(no_vault.outcome, RunOutcome::Blocked);
    let approved = request.action.clone();
    let Some(RemoteAction::Privileged(transfer)) = &mut Arc::make_mut(&mut request.action).remote
    else {
        unreachable!()
    };
    transfer.local = profile.0.join("changed upload");
    assert_eq!(
        exec(&request, &policy, Some(good_password)).outcome,
        RunOutcome::Blocked
    );
    request.action = approved;

    assert_eq!(
        exec(&request, &policy, Some("wrong-fixture-password")).outcome,
        RunOutcome::Blocked
    );
    let uploaded = exec(&request, &policy, Some(good_password));
    assert_eq!(
        uploaded.outcome,
        RunOutcome::Success,
        "{:?}",
        uploaded.detail
    );
    fs::write(&source, b"a later job must not replace the destination").unwrap();
    assert_ne!(
        exec(&request, &policy, Some(good_password)).outcome,
        RunOutcome::Success
    );

    request.action = Arc::new(action("download", &destination));
    policy.privileged = Some(
        approve_fixture(
            &profile.0,
            &request.job,
            Arc::make_mut(&mut request.action),
            &pins,
        )
        .unwrap(),
    );
    let downloaded = exec(&request, &policy, None);
    assert_eq!(
        downloaded.outcome,
        RunOutcome::Success,
        "{:?}",
        downloaded.detail
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert_eq!(exec(&request, &policy, None).outcome, RunOutcome::Blocked);
    assert_eq!(fs::read(destination).unwrap(), bytes);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_batch_runs_in_order_and_aggregates_failed_and_unpinned_targets() {
    let first = Sshd::new();
    let unpinned = Sshd::new();
    let last = Sshd::new();
    let working_directory = first.profile.0.join("batch ' directory");
    fs::create_dir(&working_directory).unwrap();
    let order = working_directory.join("target order");
    let (_, mut request, store) = scheduled_batch(
        &first.profile.0,
        &[&first, &unpinned, &last],
        r#"case "${SSH_CONNECTION##* }" in
  "$2") printf '%s\n' first > "$1"; printf '%s\n' 'secret fixture output'; exit 37 ;;
  "$3") [ "$(cat "$1")" = first ] || exit 38; printf '%s\n' last >> "$1" ;;
  *) printf '%s\n' unexpected >> "$1"; exit 39 ;;
esac"#,
        vec![
            order.file_name().unwrap().to_str().unwrap().into(),
            first.port.to_string(),
            last.port.to_string(),
        ],
    );
    Arc::make_mut(&mut request.action).working_directory =
        Some(working_directory.to_str().unwrap().into());
    let credentials = Credentials::resolve_batch(
        &request.action,
        &store,
        &ServerAccess::default(),
        |_, _, _| panic!("plain fixture key unexpectedly required the vault"),
    );
    let daemons = [first.spawn(), unpinned.spawn(), last.spawn()];
    let trust = TrustStore::from_pins(vec![first.pin(), last.pin()]).unwrap();
    let completed = batch::run(
        &request,
        None,
        &ExecutionPolicy::unrestricted(true),
        credentials,
        Ok(None),
        || Ok(trust),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Failed {
            exit_code: Some(37)
        },
        "{:?}",
        completed.detail
    );
    let detail = completed.detail.unwrap();
    assert!(
        detail.contains("1 of 3 targets succeeded · 1 failed · 1 blocked"),
        "{detail}"
    );
    assert!(detail.contains("Profile 1 failed (exit 37)"), "{detail}");
    assert!(detail.contains("Profile 2 blocked"), "{detail}");
    assert!(detail.contains("Profile 3 success"), "{detail}");
    assert!(!detail.contains("secret fixture output"));
    assert_eq!(fs::read(&order).unwrap(), b"first\nlast\n");
    for (index, daemon) in daemons.into_iter().enumerate() {
        let log = String::from_utf8(daemon.join().unwrap().stderr).unwrap();
        assert_eq!(log.contains("Accepted publickey"), index != 1, "{log}");
        if index == 1 {
            assert!(!log.contains("userauth-request"), "{log}");
        }
    }

    let directory = first.profile.0.join("batch remote directory");
    let (_, mut sftp) = scheduled_request();
    sftp.action = Arc::new(
        serde_json::from_value(json!({
            "connection": request.action.connection,
            "additional_connections": [request.action.additional_connections[1]],
            "remote": {"kind":"mkdir", "path": directory},
        }))
        .unwrap(),
    );
    let daemons = [first.spawn(), last.spawn()];
    let credentials =
        Credentials::resolve_batch(&sftp.action, &store, &ServerAccess::default(), |_, _, _| {
            panic!("plain fixture key unexpectedly required the vault")
        });
    let trust = TrustStore::from_pins(vec![first.pin(), last.pin()]).unwrap();
    let completed = batch::run(
        &sftp,
        None,
        &ExecutionPolicy::unrestricted(false),
        credentials,
        Ok(None),
        || Ok(trust),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    assert!(
        completed
            .detail
            .unwrap()
            .contains("2 of 2 targets succeeded")
    );
    assert!(directory.is_dir());
    for daemon in daemons {
        let log = String::from_utf8(daemon.join().unwrap().stderr).unwrap();
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
    }
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_batch_cancels_current_target_and_never_starts_the_next() {
    let first = Sshd::new();
    let next = Sshd::new();
    let marker = first.profile.0.join("started");
    let (scheduler, request, store) = scheduled_batch(
        &first.profile.0,
        &[&first, &next],
        "printf 'started' > \"$1\"\nexec /bin/sleep 4",
        vec![marker.to_str().unwrap().into()],
    );
    let credentials = Credentials::resolve_batch(
        &request.action,
        &store,
        &ServerAccess::default(),
        |_, _, _| panic!("plain fixture key unexpectedly required the vault"),
    );
    let trust = TrustStore::from_pins(vec![first.pin(), next.pin()]).unwrap();
    let daemon = first.spawn();
    let (sender, completion) = std::sync::mpsc::sync_channel(1);
    let id = request.id;
    let worker = thread::spawn(move || {
        let result = batch::run(
            &request,
            None,
            &ExecutionPolicy::unrestricted(true),
            credentials,
            Ok(None),
            || Ok(trust),
        );
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let started = marker.exists();
    scheduler.cancel(id).unwrap();
    let result = completion.recv_timeout(Duration::from_secs(2));
    worker.join().unwrap();
    let log = String::from_utf8(daemon.join().unwrap().stderr).unwrap();
    assert!(started, "{log}");
    let completed = result.unwrap();
    assert_eq!(completed.outcome, RunOutcome::Cancelled, "{log}");
    assert!(
        completed
            .detail
            .unwrap()
            .contains("1 cancelled · 1 not started")
    );
    assert_eq!(
        next.listener.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduler_uses_copied_registered_key_and_blocks_changed_jump_before_dial() {
    let target = Sshd::new();
    let jump = Sshd::new();
    let root = &target.profile.0;
    let passphrase = "registered-key-fixture-passphrase";
    let output = Command::new("ssh-keygen")
        .args(["-q", "-p", "-P", "", "-N", passphrase, "-f"])
        .arg(&target.user_key)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.status);

    let mut vault = Vault::create(root.join("vault"), b"scheduler-registered-key-test").unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Registered key fixture".into(),
                username: target.user.clone(),
                server_pattern: "127.0.0.1".into(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new(passphrase.into()),
        )
        .unwrap();
    let mut catalog = Catalog::open(root.clone()).unwrap();
    let mut added = catalog
        .apply(vec![
            Edit::Add(KeyDraft {
                name: "Encrypted copy".into(),
                description: String::new(),
                input: KeyInput::Copy(target.user_key.clone()),
                passphrase: Some(credential.clone()),
            }),
            Edit::Add(KeyDraft {
                name: "Jump key".into(),
                description: String::new(),
                input: KeyInput::External(jump.user_key.clone()),
                passphrase: None,
            }),
        ])
        .unwrap()
        .added
        .into_iter();
    let target_key = added.next().unwrap();
    let jump_key = added.next().unwrap();
    fs::copy(&jump.user_key, &target.user_key).unwrap();

    let registered = |server: &Sshd, key| SavedLogin {
        auth: Auth::StoredKey(key),
        ..server.saved_login(None)
    };
    let profile = Connection {
        id: 1,
        name: "Registered keys through a jump".into(),
        tag: String::new(),
        login: registered(&target, target_key),
        jump: Some(registered(&jump, jump_key)),
        emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
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
    let reference = ConnectionRef::from(&profile);
    let (_, mut request) = scheduled_request();
    request.action = Arc::new(
        serde_json::from_value(json!({ "connection": reference, "command": "true" })).unwrap(),
    );
    let prepare = || {
        let catalog = Arc::new(Catalog::open(root.clone()).unwrap());
        Credentials::resolve_for(
            Some(&reference),
            &profile,
            false,
            &mut Some(Ok(catalog)),
            &crate::clipboard_policy::ServerAccess::default(),
            |id, kind, endpoint| {
                assert_eq!(id, credential);
                assert!(matches!(kind, Kind::KeyPassphrase));
                assert_eq!(endpoint, &profile.login.endpoint);
                resolve_credential(&vault, id, kind, endpoint)
            },
        )
        .map(Some)
    };
    let ready = prepare();
    let staged = prepare();
    assert!(ready.is_ok() && staged.is_ok());
    let daemons = [jump.spawn(), target.spawn()];
    let pins = TrustStore::from_pins(vec![target.pin(), jump.pin()]).unwrap();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(true),
        ready,
        Ok(None),
        || Ok(pins),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    for daemon in daemons {
        let log = String::from_utf8(daemon.join().unwrap().stderr).unwrap();
        assert!(log.contains("Accepted publickey"), "{log}");
        assert!(!log.contains(passphrase));
    }

    catalog.apply(vec![Edit::Remove(jump_key)]).unwrap();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(true),
        staged,
        Ok(None),
        || TrustStore::from_pins(vec![target.pin(), jump.pin()]),
    );
    assert_eq!(completed.outcome, RunOutcome::Blocked);
    assert_eq!(
        completed.detail.as_deref(),
        Some("A registered SSH key changed or is unavailable.")
    );
    for server in [&target, &jump] {
        assert_eq!(
            server.listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }
    assert!(matches!(prepare(), Err("A registered SSH key is missing.")));
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduler_uses_a_saved_jump_and_vault_key_without_bypassing_pins() {
    let target = Sshd::new();
    let jump = Sshd::new();
    let root = &target.profile.0;
    let passphrase = "vault-fixture-passphrase";
    let changed = Command::new("ssh-keygen")
        .args(["-q", "-p", "-P", "", "-N", passphrase, "-f"])
        .arg(&target.user_key)
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let vault_path = root.join("vault");
    let master_password = b"scheduler-fixture-master-password";
    let mut vault = Vault::create(&vault_path, master_password).unwrap();
    let credential = vault
        .save(
            CredentialInput {
                label: "Encrypted target key".into(),
                username: target.user.clone(),
                server_pattern: "127.0.0.1".into(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            },
            Secret::new(passphrase.into()),
        )
        .unwrap();
    drop(vault);
    let vault = Vault::unlock(&vault_path, master_password).unwrap();
    let connection_path = root.join("connections.json");
    let id = Store::open(connection_path.clone())
        .unwrap()
        .save(Connection {
            id: 0,
            name: "Target via jump".into(),
            tag: String::new(),
            login: target.saved_login(Some(credential)),
            jump: Some(jump.saved_login(None)),
            emulation: kortty_terminal::TerminalProfile::Xterm.term_name().into(),
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
        })
        .unwrap();
    let store = Store::open(connection_path).unwrap();
    let profile = store.get(id).unwrap();
    let reference = ConnectionRef::from(profile);
    let login = |action: &SshCommand| {
        Credentials::resolve(
            action,
            profile,
            &ServerAccess::default(),
            |id, kind, endpoint| resolve_credential(&vault, id, kind, endpoint),
        )
        .map(Some)
    };
    let (target_endpoint, target_key) = target.pin();
    let (jump_endpoint, jump_key) = jump.pin();
    let pins = TrustStore::from_pins(vec![
        (target_endpoint.clone(), target_key.clone()),
        (jump_endpoint.clone(), jump_key.clone()),
    ])
    .unwrap();

    let (_, mut request) = scheduled_request();
    request.action = Arc::new(
        serde_json::from_value(json!({ "connection": reference, "command": "exit 23" })).unwrap(),
    );
    let jump_server = jump.spawn();
    let target_server = target.spawn();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(true),
        login(&request.action),
        Ok(None),
        || Ok(pins.clone()),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Failed {
            exit_code: Some(23)
        },
        "{:?}",
        completed.detail
    );
    for daemon in [jump_server, target_server] {
        let log = String::from_utf8(daemon.join().unwrap().stderr).unwrap();
        assert!(log.contains("Accepted publickey"), "{log}");
        assert!(!log.contains(passphrase), "fixture passphrase was logged");
    }

    let path = root.join("created through jump");
    request.action = Arc::new(
        serde_json::from_value(
            json!({ "connection": reference, "remote": {"kind":"mkdir", "path":path} }),
        )
        .unwrap(),
    );
    let jump_server = jump.spawn();
    let target_server = target.spawn();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(false),
        login(&request.action),
        Ok(None),
        || Ok(pins),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    assert!(path.is_dir());
    jump_server.join().unwrap();
    let log = String::from_utf8(target_server.join().unwrap().stderr).unwrap();
    assert!(
        log.contains("Accepted publickey") && log.contains("subsystem"),
        "{log}"
    );

    let path = root.join("must not be created");
    request.action = Arc::new(
        serde_json::from_value(
            json!({ "connection": reference, "remote": {"kind":"mkdir", "path":path} }),
        )
        .unwrap(),
    );
    let changed_target = TrustStore::from_pins(vec![
        (target_endpoint.clone(), jump_key.clone()),
        (jump_endpoint.clone(), jump_key),
    ])
    .unwrap();
    let jump_server = jump.spawn();
    let target_server = target.spawn();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(false),
        login(&request.action),
        Ok(None),
        || Ok(changed_target),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Blocked,
        "{:?}",
        completed.detail
    );
    assert_eq!(
        completed.detail.as_deref(),
        Some("SSH host key has changed")
    );
    assert!(!path.exists());
    jump_server.join().unwrap();
    let log = String::from_utf8(target_server.join().unwrap().stderr).unwrap();
    assert!(!log.contains("userauth-request"), "{log}");

    let changed_jump = TrustStore::from_pins(vec![
        (target_endpoint, target_key.clone()),
        (jump_endpoint, target_key),
    ])
    .unwrap();
    let jump_server = jump.spawn();
    let completed = run(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(false),
        login(&request.action),
        Ok(None),
        || Ok(changed_jump),
    );
    assert_eq!(completed.outcome, RunOutcome::Blocked);
    assert!(!path.exists());
    let log = String::from_utf8(jump_server.join().unwrap().stderr).unwrap();
    assert!(!log.contains("userauth-request"), "{log}");
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_executes_inline_and_linked_scripts_with_literal_argv() {
    let fixture = Sshd::new();
    let Sshd {
        profile,
        user_key,
        user,
        trust,
        port,
        ..
    } = &fixture;
    let recording = profile.0.join("recordings");
    let working_directory = profile.0.join("job's literal $(directory)");
    fs::create_dir(&working_directory).unwrap();

    let script = r#"cat <<'inner' >/dev/null
KORTTY_SCRIPT
KORTTY_SCRIPT_
inner
[ "$#" = 5 ] || exit 80
if [ "$5" = 'pty' ]; then
    [ -t 1 ] || exit 81
else
    [ ! -t 1 ] || exit 82
fi
printf '%s\0' "$1" "$2" "$3" > "$4"
printf '%s\n' 'scheduler-script-output'
exit 23"#;
    let expected = [
        "two words",
        "'\"; $HOME $(printf unsafe) `printf unsafe`",
        "--help",
    ];
    let snippets_path = profile.0.join("snippets.json");
    let mut library = Library::open(snippets_path.clone()).unwrap();
    let input = |name: &str, content: &str| Input {
        name: name.into(),
        language: "bash".into(),
        category: String::new(),
        content: content.into(),
    };
    let snippet_id = library.save(None, input("Old version", "exit 99")).unwrap();
    let linked_script = script.replace("exit 23", "exit ${EXIT_CODE}");
    library
        .save(Some(snippet_id), input("Renamed", &linked_script))
        .unwrap();
    library
        .save_variable(
            None,
            Variable {
                name: "exit_code".into(),
                value: "23".into(),
            },
        )
        .unwrap();
    for pty in [false, true] {
        let daemon = fixture.spawn();
        let output = if pty { "pty-argv" } else { "exec-argv" };
        let (_, mut request) = scheduled_request();
        let arguments = json!([
            expected[0],
            expected[1],
            expected[2],
            output,
            if pty { "pty" } else { "exec" }
        ]);
        let mut action = json!({
            "host": "127.0.0.1", "port": port, "username": user, "key_file": user_key,
            "virtual_terminal": pty.then(virtual_terminal::Options::default),
            "working_directory": working_directory,
        });
        if pty {
            action["snippet"] = json!({"id": snippet_id, "arguments": arguments});
        } else {
            action["script"] = json!({"language": "bash", "body": script, "arguments": arguments});
        }
        request.action = Arc::new(serde_json::from_value::<SshCommand>(action).unwrap());
        let completed = run_command(
            &request,
            Some(recording.clone()),
            Some(&snippets_path),
            &ExecutionPolicy::unrestricted(true),
            Ok(None),
            || Ok(trust.clone()),
            None,
        );
        let result = daemon.join().unwrap();
        let log = String::from_utf8_lossy(&result.stderr);
        let completed = completed
            .unwrap_or_else(|failure| panic!("Scheduled script failed: {}\n{log}", failure.detail));
        assert_eq!(
            completed.outcome,
            RunOutcome::Failed {
                exit_code: Some(23)
            },
            "{log}"
        );
        let actual = fs::read(working_directory.join(output)).unwrap();
        assert_eq!(
            actual
                .strip_suffix(&[0])
                .unwrap()
                .split(|byte| *byte == 0)
                .collect::<Vec<_>>(),
            expected.map(str::as_bytes)
        );
        assert_eq!(completed.recording.is_some(), pty);
        if let Some(token) = completed.recording {
            let path = virtual_terminal::path_for(&recording, request.id, token);
            assert!(kortty_recording::Replay::open(&path).unwrap().is_complete());
        }
        assert!(log.contains("Accepted publickey"), "{log}");
    }

    let daemon = fixture.spawn();
    let (_, mut request) = scheduled_request();
    request.action = Arc::new(
        serde_json::from_value(json!({
            "host": "127.0.0.1", "port": port, "username": user, "key_file": user_key,
            "working_directory": working_directory.join("missing"),
            "command": "true\nexit 77",
        }))
        .unwrap(),
    );
    let completed = run_command(
        &request,
        None,
        None,
        &ExecutionPolicy::unrestricted(true),
        Ok(None),
        || Ok(trust.clone()),
        None,
    )
    .unwrap_or_else(|failure| panic!("Failed to start working directory test: {}", failure.detail));
    assert!(matches!(
        completed.outcome,
        RunOutcome::Failed { exit_code: Some(code) } if code != 77
    ));
    assert!(String::from_utf8_lossy(&daemon.join().unwrap().stderr).contains("Accepted publickey"));
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_runs_scheduled_sftp_metadata_actions_with_strict_host_keys() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let checked = |remote, expected| {
        let (completion, log) = fixture.run_sftp(remote, fixture.trust.clone(), true);
        assert_eq!(
            completion.outcome, expected,
            "{:?}\n{log}",
            completion.detail
        );
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem") && log.contains("sftp"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completion
    };

    let folder = root.join("scheduled/deep");
    let create = json!({"kind":"mkdir", "path":folder});
    let (blocked, log) = fixture.run_sftp(
        create.clone(),
        TrustStore::from_pins(Vec::new()).unwrap(),
        true,
    );
    assert_eq!(blocked.outcome, RunOutcome::Blocked, "{log}");
    assert!(!log.contains("Accepted publickey"), "{log}");
    assert!(!folder.exists());
    checked(create.clone(), RunOutcome::Success);
    checked(create, RunOutcome::Success);
    assert!(folder.is_dir());

    let source = root.join("source");
    let destination = folder.join("renamed");
    fs::write(&source, b"original").unwrap();
    checked(
        json!({"kind":"rename", "from":source, "to":destination}),
        RunOutcome::Success,
    );
    assert!(!source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    fs::write(&source, b"collision").unwrap();
    let collision = checked(
        json!({"kind":"rename", "from":source, "to":destination}),
        RunOutcome::Failed { exit_code: None },
    );
    assert!(collision.detail.unwrap().contains("already exists"));
    assert_eq!(fs::read(&source).unwrap(), b"collision");
    assert_eq!(fs::read(&destination).unwrap(), b"original");

    checked(
        json!({"kind":"chmod", "path":destination, "mode":0o640}),
        RunOutcome::Success,
    );
    let mode = || fs::metadata(&destination).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(), 0o640);
    let alias = root.join("alias");
    symlink(&destination, &alias).unwrap();
    checked(
        json!({"kind":"chmod", "path":alias, "mode":0o777}),
        RunOutcome::Failed { exit_code: None },
    );
    assert_eq!(mode(), 0o640);
    let alias = root.join("directory-alias");
    symlink(&folder, &alias).unwrap();
    checked(
        json!({"kind":"mkdir", "path":alias.join("escaped")}),
        RunOutcome::Failed { exit_code: None },
    );
    assert!(!folder.join("escaped").exists());
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_diagnostics_reports_masked_advice_and_blocked_target() {
    use crate::local_ai::test_support::{
        fixture_http_response, fixture_response, start_test_double,
    };
    use crate::scheduler_window::{Message, Page, SchedulerWindow, agent};
    use crate::terminal_agent::swarm::headless::{Execution, Review};
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

    let server = Sshd::new();
    let blocked = Sshd::new();
    let root = &server.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&server, &blocked], "true", Vec::new());
    let references: Vec<_> = seed.action.targets().cloned().collect();
    let trust = TrustStore::from_pins(vec![server.pin(), blocked.pin()]).unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": references[0],
        "additional_connections": [references[1]],
        "working_directory": root,
        "agent": {"kind":"swarm","probe":"system","profile":9,"goal":"Summarize the observed system. fixture-sensitive-value"}
    })).unwrap();
    let path = root.join("diagnostics-jobs.json");
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let job = Job::new(
            JobId::new("diagnostics").unwrap(),
            "Swarm diagnostics",
            action,
        )
        .unwrap();
        store
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();
    }
    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Page(Page::Action));
    let review = runtime.begin_agent_review(&view).unwrap();
    let profile = agent::ProfileChoice {
        id: crate::ai_manager::ProfileRef::Numeric(9),
        name: "Fixture".into(),
        model: "fixture:local".into(),
        destination: "Loopback test endpoint".into(),
        policy_model_fingerprint: None,
        reviewed_https: false,
        reviewed_cli: None,
    };
    let targets = references
        .iter()
        .map(|reference| {
            agent::Target::capture(reference, connections.get(reference.id).unwrap(), &trust)
                .unwrap()
        })
        .collect();
    runtime
        .finish_agent_review(&mut view, review, profile, targets)
        .unwrap();
    runtime.update_view(&mut view, Message::ApproveAgent);
    runtime.update_view(&mut view, Message::Save);
    drop(runtime);

    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    let request = runtime
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(runtime.agent_run_approved(&request));
    let run = request.id;
    let reference = &references[0];
    let connection = connections.get(reference.id).unwrap();
    let credentials = Credentials::resolve_directory_browser(
        reference,
        connection,
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    let execution = Execution {
        review: Arc::new(
            Review::capture(reference.clone(), root.to_str().unwrap().into(), &trust).unwrap(),
        ),
        current: reference.clone(),
        credentials,
        startup: connection.startup,
    };
    let answer = json!({"status":"done","summary":"System observed","userMessage":"Advisory diagnostic summary. fixture-sensitive-value"}).to_string();
    let (transport, model) = start_test_double(vec![
        fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"code":"unsupported_response_format"}}"#,
        ),
        fixture_response("200 OK", "", &answer),
    ]);
    let context = super::super::agent::Context {
        transport,
        context_limit: 8192,
        targets: vec![Ok(execution), Err("Vault locked")],
        target_vault_revisions: Vec::new(),
        connection_store: connections.path().to_owned(),
        trust,
        log_policy: Default::default(),
        mask: |redactor, input| {
            redactor
                .journal_text(input, Instant::now() + Duration::from_secs(1))
                .map(|masked| masked.replace("fixture-sensitive-value", "***"))
                .ok_or_else(|| "fixture mask failed".into())
        },
        proposal: None,
        journal: None,
    };
    let daemon = server.spawn();
    let (authority, resends) = async_channel::bounded(1);
    let done = super::super::agent::spawn(request, Ok(context), authority);
    let resend = resends.recv_blocking().unwrap();
    assert!(runtime.active_agent_run_approved(&resend.scope.request));
    assert_eq!(resend.scope.evidence_targets, vec![reference.id]);
    resend.settle(Ok(()));
    let result = done.recv_blocking().unwrap();
    let ssh = daemon.join().unwrap();
    let requests = model.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["messages"], requests[1]["messages"]);
    assert!(requests[0].get("response_format").is_some());
    assert!(requests[1].get("response_format").is_none());
    assert_eq!(
        result.outcome,
        RunOutcome::Blocked,
        "{:?}\n{}",
        result.detail,
        String::from_utf8_lossy(&ssh.stderr)
    );
    assert!(
        requests[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("no execution capability")
    );
    assert!(
        requests[0]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains(root.to_str().unwrap())
    );
    assert!(!requests[0].to_string().contains("fixture-sensitive-value"));
    runtime.update_runtime(Message::Finished(result));
    drop(runtime);
    let store = SchedulerStore::open(&path).unwrap();
    let detail = store.journal_detail(run).unwrap();
    assert!(detail.contains("Probe completed"));
    assert!(detail.contains("Vault locked"));
    assert!(detail.contains("Advisory diagnostic summary."));
    assert!(!detail.contains("fixture-sensitive-value"));
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_swarm_executes_only_the_exact_proposal_once() {
    use std::io::Write as _;

    use iced::futures::{StreamExt as _, executor::block_on};
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

    use crate::local_ai::test_support::{
        accept, fixture_http_response, fixture_response, listener, request as model_request,
    };
    use crate::scheduler_window::{Message, Page, SchedulerWindow, agent};
    use crate::terminal_agent::swarm::headless::{Execution, Review};
    use crate::terminal_logging::{Policy, Redactor, Secrets};

    fn mask(redactor: &Redactor, input: &str) -> Result<String, String> {
        redactor
            .journal_text(input, Instant::now() + Duration::from_secs(1))
            .map(|masked| masked.to_string())
            .ok_or_else(|| "fixture mask failed".into())
    }

    fn changed_mask(redactor: &Redactor, input: &str) -> Result<String, String> {
        mask(redactor, input).map(|masked| masked.replace("first ' value", "***"))
    }

    let first = Sshd::new();
    let second = Sshd::new();
    let root = &first.profile.0;
    let cwd = root.join("proposal ' working directory");
    fs::create_dir(&cwd).unwrap();
    let marker = cwd.join("proposal order");
    fs::write(cwd.join("private-output.txt"), "fixture-secret-output\n").unwrap();
    let (_, seed, connections) = scheduled_batch(root, &[&first, &second], "true", Vec::new());
    let references: Vec<_> = seed.action.targets().cloned().collect();
    let trust = TrustStore::from_pins(vec![first.pin(), second.pin()]).unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": references[0],
        "additional_connections": [references[1]],
        "working_directory": cwd,
        "agent": {"kind":"swarm","probe":"proposed","profile":9,"goal":"Run the exact reviewed fixture commands."}
    })).unwrap();
    let path = root.join("proposed-jobs.json");
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let job = Job::new(
            JobId::new("proposal").unwrap(),
            "Reviewed swarm proposal",
            action,
        )
        .unwrap();
        store
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();
    }
    let answer = json!({"commands": [
        {"target": references[0].id, "command": r#"printf '%s\n' "first ' value" > 'proposal order'; cat 'private-output.txt'"#, "purpose":"Write the first fixture marker"},
        {"target": references[1].id, "command": r#"test "$(cat 'proposal order')" = "first ' value" && printf '%s\n' second >> 'proposal order'"#, "purpose":"Verify order and append the second marker"}
    ]}).to_string();
    let (model_listener, transport) = listener();
    let model = thread::spawn(move || {
        let mut first = accept(&model_listener);
        let structured = model_request(&mut first);
        first
            .get_mut()
            .write_all(
                fixture_http_response(
                    "400 Bad Request",
                    "",
                    r#"{"error":{"code":"unsupported_response_format"}}"#,
                )
                .as_bytes(),
            )
            .unwrap();
        let mut second = accept(&model_listener);
        let plain = model_request(&mut second);
        second
            .get_mut()
            .write_all(fixture_response("200 OK", "", &answer).as_bytes())
            .unwrap();
        (model_listener, structured, plain)
    });

    let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Page(Page::Action));
    let review = runtime.begin_agent_review(&view).unwrap();
    let profile = agent::ProfileChoice {
        id: crate::ai_manager::ProfileRef::Numeric(9),
        name: "Fixture".into(),
        model: "fixture:local".into(),
        destination: "Loopback test endpoint".into(),
        policy_model_fingerprint: None,
        reviewed_https: false,
        reviewed_cli: None,
    };
    let targets = references
        .iter()
        .map(|reference| {
            agent::Target::capture(reference, connections.get(reference.id).unwrap(), &trust)
                .unwrap()
        })
        .collect();
    let prompt = agent::proposal::Prompt::new(
        transport.clone(),
        8192,
        "Run the exact reviewed fixture commands.",
        cwd.to_str().unwrap(),
        references.iter().map(|reference| reference.id).collect(),
        Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default()),
        mask,
    )
    .unwrap();
    runtime
        .finish_agent_proposal_review(&mut view, review, profile, targets, prompt)
        .unwrap();
    assert!(!marker.exists());
    let task = runtime.request_agent_proposal(&mut view).unwrap();
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    let mut negotiated = false;
    let mut reviewed = false;
    while let Some(action) = block_on(actions.next()) {
        match action {
            iced_runtime::Action::Output(Message::AgentProposalProgress(
                ticket,
                agent::proposal::Progress::FormatFallback(permit),
            )) => {
                assert!(!negotiated && !marker.exists());
                let authority = runtime
                    .pending_proposal_authority(&view, &ticket)
                    .unwrap()
                    .unwrap();
                assert!(
                    authority
                        .targets
                        .iter()
                        .map(|target| &target.reference)
                        .eq(&references)
                );
                permit.try_send(()).unwrap();
                permit.close();
                negotiated = true;
            }
            iced_runtime::Action::Output(message @ Message::AgentProposed(..)) => {
                assert!(negotiated);
                assert!(runtime.update_view(&mut view, message).is_empty());
                reviewed = true;
            }
            _ => {}
        }
    }
    assert!(reviewed);
    let (model_listener, mut structured, plain) = model.join().unwrap();
    assert_eq!(
        structured
            .as_object_mut()
            .unwrap()
            .remove("response_format"),
        Some(json!({"type":"json_object"}))
    );
    assert_eq!(structured, plain);
    assert!(
        plain
            .to_string()
            .contains("Run the exact reviewed fixture commands.")
    );
    assert!(!marker.exists());
    let ticket = view.proposal_command_ticket().unwrap().to_owned();
    runtime.update_view(&mut view, Message::ApproveProposalCommands(ticket));
    runtime.update_view(&mut view, Message::Save);
    drop(runtime);

    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    let request = runtime
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    assert!(runtime.agent_run_approved(&request));
    let replay_request = || RunRequest {
        id: request.id,
        job: request.job.clone(),
        action: Arc::clone(&request.action),
        trigger: request.trigger,
        started_at: request.started_at,
        cancellation: request.cancellation.clone(),
    };
    let context = |mask| super::super::agent::Context {
        transport: transport.clone(),
        context_limit: 8192,
        target_vault_revisions: Vec::new(),
        targets: references
            .iter()
            .map(|reference| {
                let connection = connections.get(reference.id).unwrap();
                let credentials = Credentials::resolve_directory_browser(
                    reference,
                    connection,
                    &ServerAccess::default(),
                    |_, _, _| Err("unexpected credential"),
                )
                .unwrap();
                Ok(Execution {
                    review: Arc::new(
                        Review::capture(reference.clone(), cwd.to_str().unwrap().into(), &trust)
                            .unwrap(),
                    ),
                    current: reference.clone(),
                    credentials,
                    startup: connection.startup,
                })
            })
            .collect(),
        connection_store: connections.path().to_owned(),
        trust: trust.clone(),
        log_policy: Default::default(),
        mask,
        proposal: runtime.agent_proposal_run(&request).unwrap(),
        journal: None,
    };
    let mut invalid_order = context(mask);
    invalid_order.proposal.as_mut().unwrap().items.swap(0, 1);
    let run = |context| {
        let (authority, _resends) = async_channel::bounded(1);
        super::super::agent::spawn(replay_request(), Ok(context), authority)
            .recv_blocking()
            .unwrap()
    };
    let denied = run(invalid_order);
    assert_eq!(denied.outcome, RunOutcome::Blocked);
    assert!(denied.detail.unwrap().contains("ordered proposal targets"));
    let denied = run(context(changed_mask));
    assert_eq!(denied.outcome, RunOutcome::Blocked);
    assert!(denied.detail.unwrap().contains("requires masking"));
    assert!(!marker.exists());
    for server in [&first, &second] {
        assert_eq!(
            server.listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }

    let replay = context(mask);
    let first_daemon = first.spawn();
    let second_daemon = second.spawn();
    let finished = run(context(mask));
    let first_log = first_daemon.join().unwrap();
    let second_log = second_daemon.join().unwrap();
    assert_eq!(
        finished.outcome,
        RunOutcome::Success,
        "{:?}\n{}\n{}",
        finished.detail,
        String::from_utf8_lossy(&first_log.stderr),
        String::from_utf8_lossy(&second_log.stderr)
    );
    let detail = finished.detail.as_deref().unwrap();
    assert_eq!(detail.matches("Reviewed command completed").count(), 2);
    assert!(!detail.contains("fixture-secret-output"));
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "first ' value\nsecond\n"
    );
    let consumed = run(replay);
    assert_eq!(consumed.outcome, RunOutcome::Blocked);
    assert!(consumed.detail.unwrap().contains("unavailable or consumed"));
    assert!(runtime.agent_proposal_run(&request).is_err());
    assert_eq!(
        model_listener.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    for server in [&first, &second] {
        assert_eq!(
            server.listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }
    runtime.update_runtime(Message::Finished(finished));
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_rsync_download_publishes_verified_new_file() {
    openssh_rsync_verified_transfer(false, false);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen, local rsync and a local account"]
fn openssh_scheduled_rsync_upload_publishes_only_the_reviewed_new_file() {
    openssh_rsync_verified_transfer(true, false);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen, local rsync and a local account"]
fn openssh_scheduled_rsync_download_publishes_a_verified_new_directory() {
    openssh_rsync_verified_transfer(false, true);
}

#[test]
#[ignore = "requires native OpenSSH, rsync, and the current korTTY server helper"]
fn openssh_scheduled_rsync_upload_publishes_a_verified_new_directory() {
    openssh_rsync_verified_transfer(true, true);
}

fn openssh_rsync_verified_transfer(upload: bool, directory: bool) {
    use crate::scheduler_window::{Message, SchedulerWindow, sync};
    use kortty_jobs::{Job, JobGraph, JobId, SchedulerStore};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let program = PathBuf::from(
        std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").expect("set KORTTY_RSYNC_TEST_PROGRAM"),
    )
    .canonicalize()
    .unwrap();
    let bridge = PathBuf::from(
        std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").expect("set KORTTY_RSYNC_TEST_BRIDGE"),
    );
    assert!(bridge.is_file());
    if upload && directory {
        let helper = PathBuf::from(
            std::env::var_os("KORTTY_RSYNC_TEST_HELPER").expect("set KORTTY_RSYNC_TEST_HELPER"),
        )
        .canonicalize()
        .unwrap();
        assert!(helper.is_file());
        let script = root.join("rsync-fixture-dispatch.sh");
        let mut command = String::new();
        crate::scheduler_window::script::shell_quote(&mut command, helper.to_str().unwrap());
        let helper_command = kortty_server_archive::rsync_tree::COMMAND;
        let sftp = [
            "/usr/libexec/openssh/sftp-server",
            "/usr/lib/openssh/sftp-server",
            "/usr/libexec/sftp-server",
        ]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .expect("OpenSSH external sftp-server for the private forced-command fixture");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$SSH_ORIGINAL_COMMAND\" in\n  '{helper_command}') exec {command} --rsync-tree-v1 ;;\n  'rsync --server '*) exec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\" ;;\n  'internal-sftp') exec {sftp} ;;\n  *) exit 1 ;;\nesac\n"
            ),
        )
        .unwrap();
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(&fixture.config)
            .unwrap();
        writeln!(config, "ForceCommand /bin/sh \"{}\"", script.display()).unwrap();
    }
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", Vec::new());
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let source = root.join("rsync-source");
    let destination = root.join("rsync-destination");
    let expected: Vec<_> = (0..196_610).map(|index| (index % 251) as u8).collect();
    let data = |path: &std::path::Path| {
        if directory {
            path.join("nested").join("payload.bin")
        } else {
            path.to_owned()
        }
    };
    if directory {
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        if !upload {
            std::os::unix::fs::symlink(source.join("nested"), source.join("link")).unwrap();
        }
    }
    fs::write(data(&source), &expected).unwrap();
    let digest = super::super::rsync::program_digest(&program).unwrap();
    let pins = sync::fingerprints(&sync::pins(reference, &fixture.trust).unwrap());
    let (kind, local, remote) = if upload {
        ("rsync_upload", &source, &destination)
    } else {
        ("rsync_download", &destination, &source)
    };
    let mut approval =
        json!({ "program": program, "digest": digest, "reference": reference, "pins": pins });
    if upload && directory {
        let inventory = super::super::rsync::upload_tree_inventory(&source).unwrap();
        approval["directory_upload"] =
            json!({ "source": source, "destination": destination, "inventory": inventory });
    } else if upload {
        let (bytes, digest) = super::super::rsync::upload_digest(&source).unwrap();
        approval["upload"] = json!({ "source": source, "destination": destination, "bytes": bytes, "digest": digest });
    } else if directory {
        approval["directory"] = json!({ "source": source, "destination": destination });
    }
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "remote": { "kind": kind, "program": program, "local": local, "remote": remote, "approval": approval, "directory": directory },
    })).unwrap();
    let path = root.join("rsync-jobs.json");
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(JobId::new("rsync").unwrap(), "Reviewed rsync file", action).unwrap(),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut scheduler = SchedulerWindow::open(path).unwrap();
    scheduler.set_execution_allowed(true);
    let mut view = scheduler.new_view();
    let request = scheduler
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let execute = |allowed| {
        run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(allowed),
            Credentials::resolve(
                &request.action,
                profile,
                &ServerAccess::default(),
                |_, _, _| Err("unexpected credential"),
            )
            .map(Some),
            Ok(None),
            || Ok(fixture.trust.clone()),
        )
    };
    assert_eq!(execute(false).outcome, RunOutcome::Blocked);
    assert!(!destination.exists());
    if directory && !upload {
        let server = fixture.spawn();
        assert_ne!(execute(true).outcome, RunOutcome::Success);
        server.join().unwrap();
        assert!(!destination.exists());
        fs::remove_file(source.join("link")).unwrap();
    }
    if upload {
        let mut changed = expected.clone();
        changed[0] ^= 0xff;
        fs::write(data(&source), &changed).unwrap();
        let server = fixture.spawn();
        let changed = execute(true);
        server.join().unwrap();
        assert_eq!(changed.outcome, RunOutcome::Blocked);
        assert!(!destination.exists());
        fs::write(data(&source), &expected).unwrap();
    }
    let server = fixture.spawn();
    let completed = execute(true);
    let log = server.join().unwrap();
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}\n{}",
        completed.detail,
        String::from_utf8_lossy(&log.stderr)
    );
    assert_eq!(fs::read(data(&destination)).unwrap(), expected);
    if directory {
        assert!(destination.join("empty").is_dir());
        assert!(!destination.join("link").exists());
    }
    fs::write(data(&destination), b"preserve destination").unwrap();
    let server = upload.then(|| fixture.spawn());
    assert_ne!(execute(true).outcome, RunOutcome::Success);
    if let Some(server) = server {
        server.join().unwrap();
    }
    assert_eq!(
        fs::read(data(&destination)).unwrap(),
        b"preserve destination"
    );
    assert_eq!(fs::read(data(&source)).unwrap(), expected);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_reviewed_copy_runs_once_in_both_directions() {
    reviewed_copy(&[
        ("upload", ReviewedCopyStart::Manual),
        ("download", ReviewedCopyStart::Manual),
    ]);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_reviewed_copy_waits_for_gate_predecessor() {
    reviewed_copy(&[("upload", ReviewedCopyStart::Gate)]);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_reviewed_copy_waits_for_chain_and_shares_one_use_approval() {
    reviewed_copy(&[
        ("upload", ReviewedCopyStart::ChainThenManual),
        ("download", ReviewedCopyStart::ManualThenChain),
    ]);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_reviewed_copy_runs_in_confirmed_cascades() {
    reviewed_copy(&[
        ("upload", ReviewedCopyStart::CascadeMember),
        ("download", ReviewedCopyStart::CascadeRoot),
    ]);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_standing_copy_uses_a_durable_calendar_budget() {
    standing_copy("upload");
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_standing_download_uses_a_durable_calendar_budget() {
    standing_copy("download");
}

fn standing_copy(direction: &str) {
    use std::time::UNIX_EPOCH;

    use jiff::Timestamp;
    use kortty_jobs::{Job, JobGraph, JobSchedule, SchedulerStore, TimeZone};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", Vec::new());
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let local = root.join("standing-local");
    let remote = root.join("standing-remote");
    let (source, target) = if direction == "upload" {
        (&local, &remote)
    } else {
        (&remote, &local)
    };
    let original_local = root.join("standing-local-original");
    for directory in [&local, &remote] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    fs::create_dir(source.join("nested")).unwrap();
    fs::write(source.join("nested/first"), b"first firing bytes").unwrap();
    fs::write(source.join("existing"), b"unapproved replacement").unwrap();
    fs::write(source.join("excluded"), b"outside the approval").unwrap();
    fs::write(target.join("existing"), b"preserved destination").unwrap();

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let first = UNIX_EPOCH + Duration::from_secs((now.as_secs() / 60 + 10) * 60);
    let day = Duration::from_secs(24 * 60 * 60);
    let firings = [0, 1, 2, 3, 4, 5].map(|offset| first + day * offset);
    let first_utc = Timestamp::try_from(first).unwrap().to_zoned(TimeZone::UTC);
    let last_utc = Timestamp::try_from(*firings.last().unwrap())
        .unwrap()
        .to_zoned(TimeZone::UTC);
    let schedule = JobSchedule::default()
        .with_active_dates(Some(first_utc.date()), Some(last_utc.date()))
        .unwrap()
        .with_fixed_times([first_utc.time()]);
    let action: SshCommand = serde_json::from_value(json!({
        "connection": reference,
        "remote": {
            "kind": "sync", "direction": direction, "local": local,
            "remote": remote, "excludes": "excluded",
            "standing": { "days": 7, "limits": {
                "max_firings": 5, "max_actions": 64, "max_bytes": 1_024
            } },
        },
    }))
    .unwrap();
    let id = JobId::new("standing-copy").unwrap();
    let path = root.join("jobs.json");
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Standing copy", action)
                .unwrap()
                .with_schedule(schedule),
        ])
        .unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }

    let open = |path: &Path, due| {
        let mut runtime = SchedulerWindow::open(path.to_owned()).unwrap();
        runtime.set_execution_allowed(true);
        runtime
            .initialize_schedules_at(TimeZone::UTC, due - Duration::from_secs(1))
            .unwrap();
        runtime
    };
    let execute = |runtime: &SchedulerWindow, request: &RunRequest<SshCommand>| {
        let sync = runtime.sync_run_context(request);
        run_saved_route(&fixture, profile, request, sync)
    };
    let assert_no_dial = || {
        assert_eq!(
            fixture.listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    };
    {
        let mut runtime = open(&path, first);
        approve_pinned_copy(&mut runtime, &id, &fixture, profile);
    }
    {
        // A copied job definition on another profile has no device-local grant.
        let restore = root.join("standing-restore");
        fs::DirBuilder::new().mode(0o700).create(&restore).unwrap();
        let restored_path = restore.join("jobs.json");
        fs::copy(&path, &restored_path).unwrap();
        let mut restored = open(&restored_path, first);
        let request = only_copy_start(
            restored.update_runtime_at(Message::Tick, first),
            &id,
            RunTrigger::Scheduled,
        );
        assert_eq!(execute(&restored, &request).outcome, RunOutcome::Blocked);
        assert_no_dial();
        assert!(!target.join("nested").exists());
    }
    let mut runtime = open(&path, first);
    assert!(
        runtime
            .update_runtime_at(Message::Tick, first - Duration::from_secs(1))
            .is_empty()
    );
    let mut runs = Vec::new();
    for (index, due) in firings.into_iter().enumerate() {
        match index {
            2 => {
                drop(runtime);
                runtime = open(&path, due);
                fs::write(
                    source.join("nested/second"),
                    b"source changed after restart",
                )
                .unwrap();
                fs::write(source.join("nested/first"), b"must not replace first").unwrap();
            }
            3 => {
                fs::write(
                    source.join("nested/a-within-cap"),
                    b"must not publish early",
                )
                .unwrap();
                fs::write(source.join("nested/z-over-cap"), [0_u8; 1_025]).unwrap();
            }
            4 => {
                fs::rename(&local, &original_local).unwrap();
                fs::DirBuilder::new().mode(0o700).create(&local).unwrap();
                fs::create_dir_all(source.join("nested")).unwrap();
                fs::write(source.join("nested/replacement"), b"local root changed").unwrap();
            }
            5 => fs::write(source.join("nested/third"), b"exhausted approval").unwrap(),
            _ => {}
        }
        assert_eq!(runtime.next_due(), Some(due));
        let request = only_copy_start(
            runtime.update_runtime_at(Message::Tick, due),
            &id,
            RunTrigger::Scheduled,
        );
        assert_eq!(request.started_at, due);
        let outcome = if index == 5 {
            let outcome = execute(&runtime, &request);
            assert_eq!(outcome.outcome, RunOutcome::Blocked);
            assert_no_dial();
            assert!(!target.join("nested/third").exists());
            outcome
        } else {
            let daemon = fixture.spawn();
            let outcome = execute(&runtime, &request);
            let log = daemon.join().unwrap();
            let expected = if index >= 3 {
                RunOutcome::Blocked
            } else {
                RunOutcome::Success
            };
            assert_eq!(
                outcome.outcome,
                expected,
                "{:?}\n{}",
                outcome.detail,
                String::from_utf8_lossy(&log.stderr)
            );
            if index == 3 {
                assert!(
                    outcome
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("per-firing byte limit")),
                    "{:?}",
                    outcome.detail
                );
                for candidate in ["nested/a-within-cap", "nested/z-over-cap"] {
                    assert!(!target.join(candidate).exists());
                    fs::remove_file(source.join(candidate)).unwrap();
                }
            } else if index == 4 {
                assert!(
                    outcome
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("standing copy folders changed")),
                    "{:?}",
                    outcome.detail
                );
                assert!(!target.join("nested/replacement").exists());
                if direction == "download" {
                    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
                }
                // Restore the authorized root so the final refusal tests budget alone.
                fs::rename(&local, root.join("standing-local-rejected")).unwrap();
                fs::rename(&original_local, &local).unwrap();
            }
            outcome
        };
        assert_eq!(
            fs::read(target.join("nested/first")).unwrap(),
            b"first firing bytes"
        );
        if index >= 2 {
            assert_eq!(
                fs::read(target.join("nested/second")).unwrap(),
                b"source changed after restart"
            );
        }
        let expected_files = if index >= 2 { 2 } else { 1 };
        assert_eq!(
            fs::read_dir(target.join("nested")).unwrap().count(),
            expected_files
        );
        assert_eq!(
            fs::read(target.join("existing")).unwrap(),
            b"preserved destination"
        );
        assert!(!target.join("excluded").exists());
        runs.push((request.id, due, outcome.outcome));
        assert!(
            runtime
                .update_runtime_at(Message::Finished(outcome), due + Duration::from_secs(1))
                .is_empty()
        );
    }
    assert!(runtime.next_due().is_none());
    drop(runtime);
    let store = SchedulerStore::open(path).unwrap();
    assert_eq!(store.journal().len(), firings.len());
    for (run, due, outcome) in runs {
        let record = store.journal_record(run).unwrap();
        assert_eq!(record.trigger, RunTrigger::Scheduled);
        assert_eq!(record.started_at, due);
        assert_eq!(record.outcome, outcome);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReviewedCopyStart {
    Manual,
    Gate,
    ChainThenManual,
    ManualThenChain,
    CascadeMember,
    CascadeRoot,
}

fn reviewed_copy(cases: &[(&str, ReviewedCopyStart)]) {
    use std::num::NonZeroU32;

    use crate::scheduler_window::RemoteAction;
    use kortty_jobs::{DependencyMode, Job, JobGraph, JobSchedule, SchedulerStore, TimeZone};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let (_, seed, connections) = scheduled_batch(root, &[&fixture], "true", Vec::new());
    let reference = seed.action.connection.as_ref().unwrap();
    let profile = connections.get(reference.id).unwrap();
    let run_action =
        |request: &RunRequest<SshCommand>, sync| run_saved_route(&fixture, profile, request, sync);
    let run_pinned = |request: &RunRequest<SshCommand>, sync| {
        let daemon = fixture.spawn();
        let outcome = run_action(request, sync);
        let log = daemon.join().unwrap();
        assert_eq!(
            outcome.outcome,
            RunOutcome::Success,
            "{:?}\n{}",
            outcome.detail,
            String::from_utf8_lossy(&log.stderr)
        );
        outcome
    };
    let assert_no_dial = || {
        assert_eq!(
            fixture.listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    };
    let date = "2030-05-04".parse().unwrap();
    let time = "10:00".parse().unwrap();
    let due: SystemTime = TimeZone::UTC
        .to_timestamp("2030-05-04T10:00".parse().unwrap())
        .unwrap()
        .into();
    let first = JobId::new("first-prerequisite").unwrap();
    let second = JobId::new("second-prerequisite").unwrap();
    for &(direction, start) in cases {
        let local = root.join(format!("{direction}-local"));
        let remote = root.join(format!("{direction}-remote"));
        for directory in [&local, &remote] {
            fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
            fs::write(
                directory.join("existing"),
                directory.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
        }
        let (source, target) = if direction == "upload" {
            (&local, &remote)
        } else {
            (&remote, &local)
        };
        let assert_no_copy = || {
            assert_no_dial();
            assert!(!target.join("nested/new").exists());
        };
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/new"), b"exact reviewed bytes").unwrap();
        fs::write(source.join("excluded"), b"not approved").unwrap();
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": { "kind": "sync", "direction": direction, "local": local, "remote": remote, "excludes": "excluded" },
        })).unwrap();
        let id = JobId::new(format!("sync-{direction}")).unwrap();
        let path = root.join(format!("{direction}.json"));
        {
            let mut store = SchedulerStore::open(&path).unwrap();
            let mut copy = Job::new(id.clone(), "Reviewed copy", action).unwrap();
            let mut jobs = Vec::new();
            let predecessors = match start {
                ReviewedCopyStart::Manual => Vec::new(),
                ReviewedCopyStart::Gate => {
                    copy = copy.with_schedule(
                        JobSchedule::default()
                            .with_active_dates(Some(date), Some(date))
                            .unwrap()
                            .with_fixed_times([time]),
                    );
                    vec![first.clone()]
                }
                ReviewedCopyStart::ChainThenManual | ReviewedCopyStart::ManualThenChain => {
                    vec![first.clone(), second.clone()]
                }
                ReviewedCopyStart::CascadeMember => vec![first.clone()],
                ReviewedCopyStart::CascadeRoot => {
                    jobs.push(
                        Job::new(first.clone(), "Successor", seed.action.as_ref().clone())
                            .unwrap()
                            .with_dependencies(DependencyMode::Gate, vec![id.clone()], None),
                    );
                    Vec::new()
                }
            };
            for predecessor in &predecessors {
                jobs.push(
                    Job::new(
                        predecessor.clone(),
                        predecessor.as_str(),
                        seed.action.as_ref().clone(),
                    )
                    .unwrap(),
                );
            }
            if !predecessors.is_empty() {
                let mode = match start {
                    ReviewedCopyStart::ChainThenManual | ReviewedCopyStart::ManualThenChain => {
                        DependencyMode::Chain
                    }
                    _ => DependencyMode::Gate,
                };
                copy = copy.with_dependencies(mode, predecessors, NonZeroU32::new(5));
            }
            jobs.push(copy);
            let graph = JobGraph::new(jobs).unwrap();
            store.commit_with_actions(&graph, &[], &[]).unwrap();
        }
        let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
        runtime.set_execution_allowed(true);
        if start == ReviewedCopyStart::ChainThenManual {
            assert!(
                complete_copy_predecessor(&mut runtime, &second, RunOutcome::Success).is_empty()
            );
            // Give the copy a previous run so the second predecessor's success
            // belongs to an earlier round, without spending the upcoming approval.
            let previous = start_copy_manually(&mut runtime, &id);
            let outcome = run_action(&previous, runtime.sync_run_context(&previous));
            assert_eq!(outcome.outcome, RunOutcome::Blocked);
            assert!(
                runtime
                    .update_runtime(Message::Finished(outcome))
                    .is_empty()
            );
            assert_no_copy();
        }
        approve_pinned_copy(&mut runtime, &id, &fixture, profile);
        drop(runtime);

        if start == ReviewedCopyStart::Gate {
            assert_gate_blocks(&path, &first, &id, due);
            assert_no_copy();
        }
        let mut runtime = SchedulerWindow::open(path.clone()).unwrap();
        runtime.set_execution_allowed(true);
        let mut request = match start {
            ReviewedCopyStart::Gate => gated_copy_start(&mut runtime, &first, &id, due),
            ReviewedCopyStart::ChainThenManual => {
                assert!(
                    complete_copy_predecessor(&mut runtime, &first, RunOutcome::Success).is_empty()
                );
                assert_no_copy();
                assert!(
                    complete_copy_predecessor(
                        &mut runtime,
                        &second,
                        RunOutcome::Failed { exit_code: Some(7) },
                    )
                    .is_empty()
                );
                assert_no_copy();
                start_chained_copy(&mut runtime, &second, &id)
            }
            ReviewedCopyStart::ManualThenChain => {
                assert!(
                    complete_copy_predecessor(&mut runtime, &second, RunOutcome::Success)
                        .is_empty()
                );
                assert_no_copy();
                start_copy_manually(&mut runtime, &id)
            }
            ReviewedCopyStart::Manual => start_copy_manually(&mut runtime, &id),
            ReviewedCopyStart::CascadeMember => {
                let root = start_confirmed_copy_cascade(&mut runtime, &first, assert_no_copy);
                let outcome = run_pinned(&root, None);
                assert_no_copy();
                only_copy_start(
                    runtime.update_runtime(Message::Finished(outcome)),
                    &id,
                    RunTrigger::Cascade,
                )
            }
            ReviewedCopyStart::CascadeRoot => {
                start_confirmed_copy_cascade(&mut runtime, &id, assert_no_copy)
            }
        };
        let context = runtime.sync_run_context(&request).unwrap();
        let Some(RemoteAction::Sync(copy)) = &request.action.remote else {
            panic!("Expected the reviewed copy action");
        };
        let receipt = context
            .store
            .load(&request.job, copy.approval.as_deref(), context.scope)
            .unwrap();
        let forged_trigger = match start {
            ReviewedCopyStart::ChainThenManual
            | ReviewedCopyStart::CascadeMember
            | ReviewedCopyStart::CascadeRoot => Some(RunTrigger::Manual),
            ReviewedCopyStart::ManualThenChain => Some(RunTrigger::Dependency),
            ReviewedCopyStart::Manual => Some(RunTrigger::Cascade),
            ReviewedCopyStart::Gate => None,
        };
        if let Some(trigger) = forged_trigger {
            let original = std::mem::replace(&mut request.trigger, trigger);
            assert!(runtime.sync_run_context(&request).is_none());
            assert_eq!(
                run_action(&request, Some(context.clone())).outcome,
                RunOutcome::Blocked
            );
            assert_no_copy();
            request.trigger = original;
        }
        let outcome = run_pinned(&request, Some(context.clone()));
        assert_eq!(
            fs::read(target.join("nested/new")).unwrap(),
            b"exact reviewed bytes"
        );
        assert_eq!(
            fs::read(target.join("existing")).unwrap(),
            target.as_os_str().as_encoded_bytes()
        );
        assert!(!target.join("excluded").exists());
        context
            .store
            .require_claimed(&request.job, &receipt)
            .unwrap();
        assert_eq!(
            run_action(&request, Some(context)).outcome,
            RunOutcome::Blocked
        );
        assert_no_dial();
        if start == ReviewedCopyStart::Gate {
            assert!(
                runtime
                    .update_runtime_at(Message::Finished(outcome), due + Duration::from_secs(1))
                    .is_empty()
            );
            assert!(runtime.next_due().is_none());
            drop(runtime);
            let store = SchedulerStore::open(path).unwrap();
            let record = store.journal_record(request.id).unwrap();
            assert_eq!(record.trigger, RunTrigger::Scheduled);
            assert_eq!(record.outcome, RunOutcome::Success);
            continue;
        }
        if start != ReviewedCopyStart::Manual {
            let starts = runtime.update_runtime(Message::Finished(outcome));
            if start == ReviewedCopyStart::CascadeRoot {
                let successor = only_copy_start(starts, &first, RunTrigger::Cascade);
                let outcome = run_pinned(&successor, None);
                assert!(
                    runtime
                        .update_runtime(Message::Finished(outcome))
                        .is_empty()
                );
            } else {
                assert!(starts.is_empty());
            }
            let other = match start {
                ReviewedCopyStart::ManualThenChain => {
                    assert!(
                        complete_copy_predecessor(&mut runtime, &first, RunOutcome::Success)
                            .is_empty()
                    );
                    assert_no_dial();
                    start_chained_copy(&mut runtime, &second, &id)
                }
                _ => start_copy_manually(&mut runtime, &id),
            };
            let context = runtime.sync_run_context(&other).unwrap();
            let outcome = run_action(&other, Some(context));
            assert_eq!(outcome.outcome, RunOutcome::Blocked);
            assert_no_dial();
            assert!(
                runtime
                    .update_runtime(Message::Finished(outcome))
                    .is_empty()
            );
        }
    }
}

fn approve_pinned_copy(
    runtime: &mut SchedulerWindow,
    job: &JobId,
    fixture: &Sshd,
    profile: &Connection,
) {
    use crate::scheduler_window::Page;

    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Select(job.clone()));
    runtime.update_view(&mut view, Message::Page(Page::Action));
    let request = runtime.begin_sync_review(&mut view).unwrap();
    let ticket = request.ticket.clone();
    let credentials = Credentials::resolve_directory_browser(
        &request.connection,
        profile,
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    let daemon = fixture.spawn();
    let trust = fixture.trust.clone();
    let preview =
        super::super::remote::sync::preview_with_trust(request, credentials, move || Ok(trust))
            .recv_blocking()
            .unwrap();
    let log = daemon.join().unwrap();
    assert!(
        preview.is_ok(),
        "{:?}\n{}",
        preview.err(),
        String::from_utf8_lossy(&log.stderr)
    );
    runtime.update_view(
        &mut view,
        Message::SyncPreviewed(ticket, preview.map(Arc::new)),
    );
    runtime.update_view(&mut view, Message::ApproveSync);
    runtime.update_view(&mut view, Message::Save);
}

fn run_saved_route(
    fixture: &Sshd,
    profile: &Connection,
    request: &RunRequest<SshCommand>,
    sync: Option<crate::scheduler_window::sync::RunContext>,
) -> Completion {
    let policy = ExecutionPolicy {
        sync,
        ..ExecutionPolicy::unrestricted(true)
    };
    run(
        request,
        None,
        None,
        &policy,
        Credentials::resolve(
            &request.action,
            profile,
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .map(Some),
        Ok(None),
        || Ok(fixture.trust.clone()),
    )
}

fn start_confirmed_copy_cascade(
    runtime: &mut SchedulerWindow,
    root: &JobId,
    assert_no_copy: impl Fn(),
) -> RunRequest<SshCommand> {
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Select(root.clone()));
    assert!(
        runtime
            .update_view(&mut view, Message::Run(true))
            .is_empty()
    );
    assert!(!runtime.has_active_runs());
    runtime.update_view(&mut view, Message::Dismiss);
    assert!(runtime.update_view(&mut view, Message::Confirm).is_empty());
    assert!(!runtime.has_active_runs());
    assert_no_copy();
    assert!(
        runtime
            .update_view(&mut view, Message::Run(true))
            .is_empty()
    );
    assert!(!runtime.has_active_runs());
    assert_no_copy();
    only_copy_start(
        runtime.update_view(&mut view, Message::Confirm),
        root,
        RunTrigger::Cascade,
    )
}

fn start_copy_manually(runtime: &mut SchedulerWindow, job: &JobId) -> RunRequest<SshCommand> {
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Select(job.clone()));
    let mut starts = runtime.update_view(&mut view, Message::Run(false));
    if starts.is_empty() {
        starts = runtime.update_view(&mut view, Message::Confirm);
    }
    only_copy_start(starts, job, RunTrigger::Manual)
}

fn start_chained_copy(
    runtime: &mut SchedulerWindow,
    predecessor: &JobId,
    job: &JobId,
) -> RunRequest<SshCommand> {
    let starts = complete_copy_predecessor(runtime, predecessor, RunOutcome::Success);
    only_copy_start(starts, job, RunTrigger::Dependency)
}

fn only_copy_start(
    mut starts: Vec<RunRequest<SshCommand>>,
    job: &JobId,
    trigger: RunTrigger,
) -> RunRequest<SshCommand> {
    assert_eq!(starts.len(), 1);
    let request = starts.pop().unwrap();
    assert_eq!(&request.job, job);
    assert_eq!(request.trigger, trigger);
    request
}

fn complete_copy_predecessor(
    runtime: &mut SchedulerWindow,
    predecessor: &JobId,
    outcome: RunOutcome,
) -> Vec<RunRequest<SshCommand>> {
    let completed = copy_predecessor_finished(runtime, predecessor, outcome);
    runtime.update_runtime(completed)
}

fn copy_predecessor_finished(
    runtime: &mut SchedulerWindow,
    predecessor: &JobId,
    outcome: RunOutcome,
) -> Message {
    Message::Finished(Completion {
        id: start_copy_manually(runtime, predecessor).id,
        outcome,
        detail: None,
        recording: None,
        swarm_report: None,
    })
}

fn assert_gate_blocks(path: &Path, predecessor: &JobId, job: &JobId, due: SystemTime) {
    use kortty_jobs::{BlockReason, Blocker, SchedulerStore, TimeZone};

    // Exercise separate histories at the same occurrence with one shared receipt.
    let blocked_path = path.with_file_name("gate-blocked.json");
    fs::copy(path, &blocked_path).unwrap();
    let receipts = path.with_file_name("job-sftp-reviews.json");
    let before = fs::read(&receipts).unwrap();
    let mut blocked = SchedulerWindow::open(blocked_path.clone()).unwrap();
    blocked.set_execution_allowed(true);
    blocked
        .initialize_schedules_at(TimeZone::UTC, due - Duration::from_secs(240))
        .unwrap();
    assert_eq!(blocked.next_due(), Some(due));
    let failed = RunOutcome::Failed { exit_code: Some(7) };
    let completed = copy_predecessor_finished(&mut blocked, predecessor, failed);
    assert!(
        blocked
            .update_runtime_at(completed, due - Duration::from_secs(120))
            .is_empty()
    );
    assert!(blocked.update_runtime_at(Message::Tick, due).is_empty());
    assert!(blocked.next_due().is_none());
    drop(blocked);
    let store = SchedulerStore::open(blocked_path).unwrap();
    let record = store.latest_finished(job).unwrap();
    assert_eq!(record.trigger, RunTrigger::Scheduled);
    assert_eq!(record.outcome, RunOutcome::Blocked);
    assert_eq!(
        record.blockers.as_ref(),
        [Blocker {
            predecessor: predecessor.clone(),
            reason: BlockReason::NotSuccessful(failed),
        }]
    );
    assert!(fs::read(receipts).unwrap() == before);
}

fn gated_copy_start(
    runtime: &mut SchedulerWindow,
    predecessor: &JobId,
    job: &JobId,
    due: SystemTime,
) -> RunRequest<SshCommand> {
    use kortty_jobs::TimeZone;

    runtime
        .initialize_schedules_at(TimeZone::UTC, due - Duration::from_secs(240))
        .unwrap();
    assert_eq!(runtime.next_due(), Some(due));
    let completed = copy_predecessor_finished(
        runtime,
        predecessor,
        RunOutcome::Failed { exit_code: Some(7) },
    );
    assert!(
        runtime
            .update_runtime_at(completed, due - Duration::from_secs(120))
            .is_empty()
    );
    let completed = copy_predecessor_finished(runtime, predecessor, RunOutcome::Success);
    assert!(
        runtime
            .update_runtime_at(completed, due - Duration::from_secs(60))
            .is_empty()
    );
    assert!(
        runtime
            .update_runtime_at(Message::Tick, due - Duration::from_secs(1))
            .is_empty()
    );
    let request = only_copy_start(
        runtime.update_runtime_at(Message::Tick, due),
        job,
        RunTrigger::Scheduled,
    );
    assert!(runtime.update_runtime_at(Message::Tick, due).is_empty());
    request
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_upload_and_download_publish_complete_private_files() {
    let fixture = Sshd::new();
    let local = fixture.profile.0.join("local");
    let remote = fixture.profile.0.join("remote");
    fs::create_dir(&local).unwrap();
    fs::create_dir(&remote).unwrap();
    let source = local.join("source");
    let uploaded = remote.join("uploaded");
    let downloaded = local.join("downloaded");
    let payload: Vec<u8> = (0..180_000).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &payload).unwrap();
    let run_job = |action: serde_json::Value, outcome: RunOutcome| {
        let (completed, log) = fixture.run_sftp(action, fixture.trust.clone(), true);
        assert_eq!(completed.outcome, outcome, "{:?}\n{log}", completed.detail);
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        completed
    };
    let upload = json!({ "kind":"upload", "local":source, "remote":uploaded });
    let download = json!({ "kind":"download", "local":downloaded, "remote":uploaded });
    let upload_result = run_job(upload.clone(), RunOutcome::Success);
    assert_eq!(
        upload_result.detail.as_deref(),
        Some("Uploaded 180000 bytes via SFTP")
    );
    assert_eq!(fs::read(&uploaded).unwrap(), payload);
    assert_eq!(
        fs::metadata(&uploaded).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&remote).unwrap().count(), 1);

    fs::write(&source, b"new local bytes").unwrap();
    let failed = RunOutcome::Failed { exit_code: None };
    run_job(upload, failed);
    assert_eq!(fs::read(&uploaded).unwrap(), payload);
    let download_result = run_job(download.clone(), RunOutcome::Success);
    assert_eq!(
        download_result.detail.as_deref(),
        Some("Downloaded 180000 bytes via SFTP")
    );
    assert_eq!(fs::read(&downloaded).unwrap(), payload);
    assert_eq!(
        fs::metadata(&downloaded).unwrap().permissions().mode() & 0o777,
        fs::metadata(&source).unwrap().permissions().mode() & 0o777
    );
    fs::write(&downloaded, b"keep local destination").unwrap();
    run_job(download, failed);
    assert_eq!(fs::read(&downloaded).unwrap(), b"keep local destination");
    assert_eq!(fs::read_dir(&local).unwrap().count(), 2);

    let alias = local.join("alias");
    symlink(&source, &alias).unwrap();
    let forbidden = remote.join("unpublished");
    run_job(
        json!({ "kind":"upload", "local":alias, "remote":forbidden }),
        failed,
    );
    assert!(!forbidden.exists());
    let remote_alias = remote.join("remote-alias");
    symlink(&uploaded, &remote_alias).unwrap();
    let forbidden = local.join("unpublished");
    run_job(
        json!({ "kind":"download", "local":forbidden, "remote":remote_alias }),
        failed,
    );
    assert!(!forbidden.exists());
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_creates_a_checked_zip_and_optional_local_copy() {
    use std::io::{Cursor, Read as _};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let source = root.join("source folder");
    let nested = source.join("nested");
    fs::create_dir_all(nested.join("empty")).unwrap();
    let payload: Vec<u8> = (0..91_003).map(|index| (index % 251) as u8).collect();
    fs::write(nested.join("payload"), &payload).unwrap();
    let notes = root.join("notes.txt");
    fs::write(&notes, b"checked second source").unwrap();
    let zip = source.join("backup.zip");
    let local = root.join("local backup.zip");
    let run_zip = |action, outcome| {
        let (completed, log) = fixture.run_sftp(action, fixture.trust.clone(), true);
        assert_eq!(completed.outcome, outcome, "{:?}\n{log}", completed.detail);
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completed.detail.unwrap()
    };

    let archive = json!({ "kind":"archive_zip", "sources":[source, notes], "target":zip, "level":6, "download":local });
    let detail = run_zip(archive.clone(), RunOutcome::Success);
    assert!(
        detail.contains("Created ZIP with 5 entries") && detail.contains("saved a local copy"),
        "{detail}"
    );
    let original = fs::read(&zip).unwrap();
    assert_eq!(fs::read(&local).unwrap(), original);
    for path in [&zip, &local] {
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
    }
    let mut reader = zip::ZipArchive::new(Cursor::new(&original)).unwrap();
    assert_eq!(reader.len(), 5);
    assert!(
        reader
            .by_name("source folder/nested/empty/")
            .unwrap()
            .is_dir()
    );
    let mut data = Vec::new();
    {
        let mut entry = reader.by_name("source folder/nested/payload").unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::Deflated);
        entry.read_to_end(&mut data).unwrap();
    }
    assert_eq!(data, payload);
    data.clear();
    reader
        .by_name("notes.txt")
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    assert_eq!(data, b"checked second source");
    assert_eq!(fs::read_dir(&source).unwrap().count(), 2);

    let failed = RunOutcome::Failed { exit_code: None };
    assert!(run_zip(archive, failed).contains("already exists"));
    assert_eq!(fs::read(&zip).unwrap(), original);
    let stored = root.join("stored.zip");
    run_zip(
        json!({ "kind":"archive_zip", "sources":[notes], "target":stored, "level":0 }),
        RunOutcome::Success,
    );
    let mut stored = zip::ZipArchive::new(fs::File::open(stored).unwrap()).unwrap();
    assert_eq!(
        stored.by_name("notes.txt").unwrap().compression(),
        zip::CompressionMethod::Stored
    );
    let unpublished = root.join("unpublished.zip");
    assert!(run_zip(json!({ "kind":"archive_zip", "sources":[notes], "target":unpublished, "level":0, "download":local }), failed).contains("local archive destination already exists"));
    assert!(!unpublished.exists());
    assert_eq!(fs::read(&local).unwrap(), original);

    let link = source.join("escape");
    symlink(&notes, &link).unwrap();
    run_zip(
        json!({ "kind":"archive_zip", "sources":[source], "target":unpublished, "level":6 }),
        failed,
    );
    assert!(!unpublished.exists());
    assert!(link.is_symlink());
    assert_eq!(fs::read(&notes).unwrap(), b"checked second source");

    let filtered = root.join("filtered.zip");
    run_zip(
        json!({
            "kind":"archive", "format":"zip", "sources":[source, notes], "target":filtered,
            "level":6, "excludes":["source folder/nested", "source folder/escape", "*.zip"],
        }),
        RunOutcome::Success,
    );
    let mut filtered = zip::ZipArchive::new(fs::File::open(filtered).unwrap()).unwrap();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.by_name("source folder/").unwrap().is_dir());
    assert!(filtered.by_name("notes.txt").unwrap().is_file());
    assert_eq!(fs::read(&zip).unwrap(), original);

    let oversized = root.join("oversized");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(256 * 1024 * 1024 + 1)
        .unwrap();
    let detail = run_zip(
        json!({ "kind":"archive_zip", "sources":[oversized], "target":unpublished, "level":6 }),
        RunOutcome::Blocked,
    );
    assert!(detail.contains("256 MiB"), "{detail}");
    fs::OpenOptions::new()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let detail = run_zip(
        json!({ "kind":"archive_zip", "sources":[oversized], "target":unpublished, "level":0 }),
        RunOutcome::Blocked,
    );
    assert!(detail.contains("64 MiB"), "{detail}");
    assert!(!unpublished.exists());

    let config = fs::read_to_string(&fixture.config).unwrap();
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            "Subsystem sftp internal-sftp -P hardlink",
        ),
    )
    .unwrap();
    let detail = run_zip(
        json!({ "kind":"archive_zip", "sources":[notes], "target":unpublished, "level":6 }),
        RunOutcome::Blocked,
    );
    assert!(detail.contains("hard-link"), "{detail}");
    assert!(!unpublished.exists());
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_encrypted_zip_uses_vault_password_and_authenticated_aes() {
    use std::io::{Cursor, Read};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let source = root.join("source.txt");
    let payload = b"checked archive source that should never occur in encrypted bytes";
    fs::write(&source, payload).unwrap();
    let mut vault = Vault::create(root.join("vault"), b"test-only master password").unwrap();
    let password = "test-only long archive password";
    let id = vault
        .save(
            CredentialInput {
                label: "Fixture archive".into(),
                kind: CredentialKind::ArchivePassword,
                ..Default::default()
            },
            Secret::new(password.into()),
        )
        .unwrap();

    let target = root.join("remote.zip");
    let local = root.join("local.zip");
    let action = json!({
        "kind": "archive", "format": "zip_password", "password_credential": id,
        "sources": [source], "target": target, "level": 0, "download": local,
    });
    let (completed, log) = fixture.run_sftp_with_password(
        action.clone(),
        fixture.trust.clone(),
        true,
        Ok(vault.archive_password(&id)),
    );
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}\n{log}",
        completed.detail
    );
    assert!(
        log.contains("Accepted publickey") && log.contains("subsystem"),
        "{log}"
    );
    assert!(!log.contains("Starting session: command"), "{log}");
    assert!(!log.contains(password));
    assert!(!completed.detail.unwrap().contains(password));
    let bytes = fs::read(&target).unwrap();
    assert_eq!(fs::read(&local).unwrap(), bytes);
    assert!(!bytes.windows(payload.len()).any(|bytes| bytes == payload));
    assert!(
        !bytes
            .windows(password.len())
            .any(|bytes| bytes == password.as_bytes())
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    assert_eq!(
        archive
            .get_aes_verification_key_and_salt(0)
            .unwrap()
            .unwrap()
            .aes_mode,
        zip::AesMode::Aes256,
    );
    assert!(archive.by_name("source.txt").is_err());
    let incorrect = archive
        .by_name_decrypt("source.txt", b"wrong password")
        .and_then(|mut file| Ok(file.read_to_end(&mut Vec::new())?));
    assert!(incorrect.is_err());
    let mut file = archive
        .by_name_decrypt("source.txt", password.as_bytes())
        .unwrap();
    let mut content = Vec::new();
    file.read_to_end(&mut content).unwrap();
    assert_eq!(content, payload);
    let authentication_byte = (file.data_start().unwrap() + file.compressed_size() - 1) as usize;
    drop(file);
    let mut changed = bytes.clone();
    changed[authentication_byte] ^= 1;
    let mut damaged = zip::ZipArchive::new(Cursor::new(changed)).unwrap();
    let mut file = damaged
        .by_name_decrypt("source.txt", password.as_bytes())
        .unwrap();
    assert!(file.read_to_end(&mut Vec::new()).is_err());

    let (completed, _) = fixture.run_sftp_with_password(
        action,
        fixture.trust.clone(),
        true,
        Ok(vault.archive_password(&id)),
    );
    assert_eq!(completed.outcome, RunOutcome::Failed { exit_code: None });
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(fs::read(&local).unwrap(), bytes);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_creates_tar_and_tar_bz2_with_bounded_exclusions() {
    use std::io::{Cursor, Read};

    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let source = root.join("source");
    let nested = source.join("nested");
    let cache = nested.join("cache");
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir(nested.join("empty")).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"outside and excluded").unwrap();
    symlink(&outside, cache.join("not-followed")).unwrap();
    fs::write(nested.join("debug.log"), b"excluded by wildcard").unwrap();
    let absolute_exclude = nested.join("abs.txt");
    fs::write(&absolute_exclude, b"excluded by absolute path").unwrap();
    let long_name = "z".repeat(160);
    let payload: Vec<u8> = (0..91_003).map(|index| (index % 251) as u8).collect();
    fs::write(nested.join(&long_name), &payload).unwrap();
    fs::write(nested.join("tail.txt"), b"tail after padding").unwrap();
    let excludes = json!(["*.log", "cache", absolute_exclude]);
    let member = format!("source/nested/{long_name}");
    for (format, extension) in [("tar", "tar"), ("tar_bz2", "tar.bz2")] {
        let target = root.join(format!("result.{extension}"));
        let local = root.join(format!("local.{extension}"));
        let archive = json!({
            "kind":"archive", "format":format, "sources":[source], "target":target,
            "level":6, "excludes":excludes, "download":local,
        });
        let (completed, log) = fixture.run_sftp(archive.clone(), fixture.trust.clone(), true);
        assert_eq!(
            completed.outcome,
            RunOutcome::Success,
            "{:?}\n{log}",
            completed.detail
        );
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        let bytes = fs::read(&target).unwrap();
        assert_eq!(fs::read(&local).unwrap(), bytes);
        for path in [&target, &local] {
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        }
        let decoded: Box<dyn Read> = if format == "tar" {
            Box::new(Cursor::new(&bytes))
        } else {
            assert!(bytes.starts_with(b"BZh6"));
            Box::new(bzip2::read::BzDecoder::new(Cursor::new(&bytes)))
        };
        let mut archive_data = tar::Archive::new(decoded);
        let mut paths = Vec::new();
        for entry in archive_data.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_owned();
            if entry.header().entry_type().is_dir() {
                assert_eq!(entry.header().mode().unwrap(), 0o700);
            } else {
                assert_eq!(entry.header().mode().unwrap(), 0o600);
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                if path == member {
                    assert_eq!(data, payload);
                } else {
                    assert_eq!(path, "source/nested/tail.txt");
                    assert_eq!(data, b"tail after padding");
                }
            }
            paths.push(path);
        }
        assert_eq!(
            paths,
            [
                "source",
                "source/nested",
                "source/nested/empty",
                "source/nested/tail.txt",
                &member
            ]
        );
        let (completed, _) = fixture.run_sftp(archive, fixture.trust.clone(), true);
        assert_eq!(completed.outcome, RunOutcome::Failed { exit_code: None });
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::read(&local).unwrap(), bytes);
    }

    let blocked = root.join("excluded.tar");
    let (completed, _) = fixture.run_sftp(
        json!({"kind":"archive", "format":"tar", "sources":[source], "target":blocked, "level":6, "excludes":["source"]}),
        fixture.trust.clone(),
        true,
    );
    assert_eq!(completed.outcome, RunOutcome::Blocked);
    assert!(!blocked.exists());
    let unsafe_archive = root.join("unsafe.tar");
    let (completed, _) = fixture.run_sftp(
        json!({"kind":"archive", "format":"tar", "sources":[source], "target":unsafe_archive, "level":6}),
        fixture.trust.clone(),
        true,
    );
    assert_eq!(completed.outcome, RunOutcome::Failed { exit_code: None });
    assert!(!unsafe_archive.exists());
    assert_eq!(fs::read(&outside).unwrap(), b"outside and excluded");
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_transfers_private_folder_trees_without_overwriting() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let local_parent = root.join("local");
    let source = local_parent.join("source folder");
    let remote_parent = root.join("remote");
    let remote = remote_parent.join("received folder");
    let download = local_parent.join("downloaded folder");
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    fs::create_dir(&remote_parent).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"keep outside tree").unwrap();
    fs::write(source.join("ordinary"), b"first file").unwrap();
    let bytes: Vec<u8> = (0..90_001).map(|index| (index % 251) as u8).collect();
    fs::write(source.join("nested/payload"), &bytes).unwrap();
    symlink(&outside, source.join("skip link")).unwrap();

    let run_job = |operation, expected| {
        let (completed, log) = fixture.run_sftp(operation, fixture.trust.clone(), true);
        assert_eq!(completed.outcome, expected, "{:?}\n{log}", completed.detail);
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completed.detail.unwrap()
    };
    let upload = json!({"kind":"upload", "local":source, "remote":remote, "folder":true});
    let pull = json!({"kind":"download", "local":download, "remote":remote, "folder":true});
    let failed = RunOutcome::Failed { exit_code: None };
    let uploaded = run_job(upload.clone(), RunOutcome::Success);
    assert!(
        uploaded.contains("3 folders · 2 files · 90011 bytes"),
        "{uploaded}"
    );
    assert!(
        uploaded.contains("1 links or special entries skipped"),
        "{uploaded}"
    );
    assert!(remote.join("nested/empty").is_dir());
    assert!(!remote.join("skip link").exists());
    assert_eq!(fs::read(remote.join("nested/payload")).unwrap(), bytes);
    assert_eq!(fs::read(remote.join("ordinary")).unwrap(), b"first file");
    let mode = |path: &Path| fs::metadata(path).unwrap().mode() & 0o777;
    assert_eq!(mode(&remote), 0o700);
    assert_eq!(mode(&remote.join("nested")), 0o700);
    assert_eq!(mode(&remote.join("nested/payload")), 0o600);
    assert_eq!(fs::read_dir(remote.join("nested")).unwrap().count(), 2);
    fs::write(source.join("ordinary"), b"replacement attempt").unwrap();
    run_job(upload, failed);
    assert_eq!(fs::read(remote.join("ordinary")).unwrap(), b"first file");

    symlink(&outside, remote.join("skip remote link")).unwrap();
    let downloaded = run_job(pull.clone(), RunOutcome::Success);
    assert!(
        downloaded.contains("1 links or special entries skipped"),
        "{downloaded}"
    );
    assert!(download.join("nested/empty").is_dir());
    assert!(!download.join("skip remote link").exists());
    assert_eq!(fs::read(download.join("nested/payload")).unwrap(), bytes);
    assert_eq!(mode(&download), mode(&source));
    assert_eq!(mode(&download.join("nested")), mode(&source.join("nested")));
    assert_eq!(
        mode(&download.join("ordinary")),
        mode(&source.join("ordinary"))
    );
    fs::write(download.join("ordinary"), b"keep local collision").unwrap();
    run_job(pull, failed);
    assert_eq!(
        fs::read(download.join("ordinary")).unwrap(),
        b"keep local collision"
    );
    assert_eq!(fs::read_dir(&local_parent).unwrap().count(), 2);
    assert_eq!(fs::read(&outside).unwrap(), b"keep outside tree");

    let config = fs::read_to_string(&fixture.config).unwrap();
    let refuse = |request: &str| {
        fs::write(
            &fixture.config,
            config.replace(
                "Subsystem sftp internal-sftp",
                &format!("Subsystem sftp internal-sftp -P {request}"),
            ),
        )
        .unwrap();
    };
    let denied = remote_parent.join("not created");
    refuse("hardlink");
    let blocked = run_job(
        json!({"kind":"upload", "local":source, "remote":denied, "folder":true}),
        RunOutcome::Blocked,
    );
    assert!(blocked.contains("hard-link"), "{blocked}");
    assert!(!denied.exists());

    refuse("write");
    let partial = remote_parent.join("private partial");
    let detail = run_job(
        json!({"kind":"upload", "local":source, "remote":partial, "folder":true}),
        failed,
    );
    assert!(detail.contains("private partial folder"), "{detail}");
    assert_eq!(mode(&partial), 0o700);
    assert!(!partial.join("ordinary").exists());
    assert!(!partial.join("nested/payload").exists());

    refuse("read");
    let unpublished = local_parent.join("unpublished folder");
    run_job(
        json!({"kind":"download", "local":unpublished, "remote":remote, "folder":true}),
        failed,
    );
    assert!(!unpublished.exists());
    assert_eq!(fs::read_dir(&local_parent).unwrap().count(), 2);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_cancels_a_stalled_subsystem() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let subsystem = root.join("stall-sftp");
    // Test double for a subsystem that accepts the SSH channel but never
    // responds to SFTP INIT. The SSH transport and authentication are real.
    fs::write(
        &subsystem,
        b"#!/bin/sh\n: > \"$0.started\"\nexec /bin/sleep 4\n",
    )
    .unwrap();
    fs::set_permissions(&subsystem, fs::Permissions::from_mode(0o700)).unwrap();
    let config = fs::read_to_string(&fixture.config).unwrap();
    let subsystem_path = subsystem
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            &format!("Subsystem sftp \"{subsystem_path}\""),
        ),
    )
    .unwrap();
    let daemon = fixture.spawn();
    let destination = root.join("must-not-be-created");
    let (scheduler, mut request) = scheduled_request();
    let id = request.id;
    request.action = Arc::new(
        serde_json::from_value(json!({
            "host": "127.0.0.1", "port": fixture.port, "username": fixture.user,
            "key_file": fixture.user_key, "remote": {"kind": "mkdir", "path": destination},
        }))
        .unwrap(),
    );
    let trust = fixture.trust.clone();
    let (finished, completion) = std::sync::mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let _ = finished.send(run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(true),
            Ok(None),
            Ok(None),
            || Ok(trust),
        ));
    });

    let started = subsystem.with_extension("started");
    let deadline = Instant::now() + Duration::from_secs(6);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let began = started.exists();
    scheduler.cancel(id).unwrap();
    let completed = completion.recv_timeout(Duration::from_secs(2));
    worker.join().unwrap();
    let output = daemon.join().unwrap();
    let log = String::from_utf8(output.stderr).unwrap();
    assert!(began, "{log}");
    assert_eq!(completed.unwrap().outcome, RunOutcome::Cancelled, "{log}");
    assert!(!destination.exists());
}

#[test]
#[ignore = "requires OpenSSH with SFTP copy-data and hard-link support"]
fn openssh_scheduled_sftp_copies_remote_folders_without_overwrite() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let source = root.join("source");
    let destination = root.join("source-copy");
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    fs::write(source.join("nested/payload"), b"folder data").unwrap();
    fs::write(source.join("zero bytes"), []).unwrap();
    let copy = |from: &Path, to: &Path| json!({ "kind": "copy_remote", "from": from, "to": to });
    let run_job = |from: &Path, to: &Path, expected| {
        let (completed, log) = fixture.run_sftp(copy(from, to), fixture.trust.clone(), false);
        assert_eq!(completed.outcome, expected, "{:?}\n{log}", completed.detail);
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completed.detail.unwrap()
    };

    let detail = run_job(&source, &destination, RunOutcome::Success);
    assert!(
        detail.contains("3 folders, 2 files and 11 bytes"),
        "{detail}"
    );
    assert!(destination.join("nested/empty").is_dir());
    assert_eq!(
        fs::read(destination.join("nested/payload")).unwrap(),
        b"folder data"
    );
    assert_eq!(
        fs::metadata(destination.join("zero bytes")).unwrap().len(),
        0
    );
    assert_eq!(
        fs::metadata(destination.join("nested/empty"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(destination.join("nested/payload"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );

    let failed = RunOutcome::Failed { exit_code: None };
    fs::write(destination.join("nested/payload"), b"keep existing data").unwrap();
    assert!(run_job(&source, &destination, failed).contains("already exists"));
    assert_eq!(
        fs::read(destination.join("nested/payload")).unwrap(),
        b"keep existing data"
    );

    for target in [&source, &source.join("nested/copy")] {
        let (_, mut request) = scheduled_request();
        request.action = Arc::new(
            serde_json::from_value(json!({
                "host": "127.0.0.1", "port": fixture.port, "username": fixture.user,
                "key_file": fixture.user_key, "remote": copy(&source, target),
            }))
            .unwrap(),
        );
        let result = run(
            &request,
            None,
            None,
            &ExecutionPolicy::unrestricted(false),
            Ok(None),
            Ok(None),
            || panic!("A copy to its own source tree must fail before accessing SSH trust"),
        );
        assert_eq!(result.outcome, RunOutcome::Blocked);
    }

    let alias = root.join("alias");
    symlink(&source, &alias).unwrap();
    assert!(run_job(&source, &alias.join("nested/copy"), failed).contains("canonical"));
    assert!(!source.join("nested/copy").exists());

    let source_link = source.join("nested/link");
    symlink(&destination, &source_link).unwrap();
    let partial = root.join("partial");
    run_job(&source, &partial, failed);
    assert!(!partial.exists());
    fs::remove_file(source_link).unwrap();

    let config = fs::read_to_string(&fixture.config).unwrap();
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            "Subsystem sftp internal-sftp -P open",
        ),
    )
    .unwrap();
    assert!(run_job(&source, &partial, failed).contains("partial destination folder"));
    assert!(partial.join("nested/empty").is_dir());
    assert!(!partial.join("nested/payload").exists());
    assert_eq!(
        fs::read(source.join("nested/payload")).unwrap(),
        b"folder data"
    );
}

#[test]
#[ignore = "requires OpenSSH with SFTP copy-data and lsetstat support"]
fn openssh_scheduled_sftp_copies_on_server_and_changes_numeric_ownership() {
    let fixture = Sshd::new();
    let root = fixture.profile.0.join("test-files");
    fs::create_dir(&root).unwrap();
    let run_job = |action, expected| {
        let (completed, log) = fixture.run_sftp(action, fixture.trust.clone(), false);
        assert_eq!(completed.outcome, expected, "{:?}\n{log}", completed.detail);
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completed.detail.unwrap()
    };

    let source = root.join("original");
    let destination = root.join("copy");
    let payload: Vec<u8> = (0..8 * 1024 * 1024 + 19)
        .map(|index| (index % 251) as u8)
        .collect();
    fs::write(&source, &payload).unwrap();
    let copy = json!({ "kind":"copy_remote", "from":source, "to":destination });
    run_job(copy.clone(), RunOutcome::Success);
    assert_eq!(fs::read(&destination).unwrap(), payload);
    assert_eq!(fs::read(&source).unwrap(), payload);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o600);
    let failed = RunOutcome::Failed { exit_code: None };
    assert!(run_job(copy, failed).contains("already exists"));
    assert_eq!(fs::read(&destination).unwrap(), payload);

    let empty = root.join("empty");
    let empty_copy = root.join("empty copy");
    fs::write(&empty, []).unwrap();
    run_job(
        json!({ "kind":"copy_remote", "from":empty, "to":empty_copy }),
        RunOutcome::Success,
    );
    assert_eq!(fs::metadata(&empty_copy).unwrap().len(), 0);
    let alias = root.join("alias");
    let unpublished = root.join("must stay absent");
    symlink(&source, &alias).unwrap();
    run_job(
        json!({ "kind":"copy_remote", "from":alias, "to":unpublished }),
        failed,
    );
    assert!(!unpublished.exists());

    let original = fs::metadata(&source).unwrap();
    let groups = Command::new("id").arg("-G").output().unwrap();
    assert!(groups.status.success());
    let group = String::from_utf8(groups.stdout)
        .unwrap()
        .split_whitespace()
        .map(|field| field.parse::<u32>().unwrap())
        .find(|gid| *gid != original.gid())
        .unwrap_or(original.gid());
    let chown = json!({ "kind":"chown", "path":source, "gid":group });
    assert_eq!(
        run_job(chown.clone(), RunOutcome::Success),
        if group == original.gid() {
            "Remote ownership already matches"
        } else {
            "Remote ownership changed"
        }
    );
    let changed = fs::metadata(&source).unwrap();
    assert_eq!((changed.uid(), changed.gid()), (original.uid(), group));
    run_job(
        json!({ "kind":"chown", "path":alias, "gid":original.gid() }),
        failed,
    );
    assert_eq!(fs::metadata(&source).unwrap().gid(), group);

    let config = fs::read_to_string(&fixture.config).unwrap();
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            "Subsystem sftp internal-sftp -P hardlink",
        ),
    )
    .unwrap();
    assert!(
        run_job(
            json!({ "kind":"copy_remote", "from":source, "to":unpublished }),
            RunOutcome::Blocked
        )
        .contains("hard-link")
    );
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            "Subsystem sftp internal-sftp -P copy-data,lsetstat",
        ),
    )
    .unwrap();
    assert!(
        run_job(
            json!({ "kind":"copy_remote", "from":source, "to":unpublished }),
            RunOutcome::Blocked
        )
        .contains("advertise")
    );
    assert!(run_job(chown, RunOutcome::Blocked).contains("advertise"));
    assert!(!unpublished.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 5);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_deletes_only_confirmed_files_or_trees() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let delete = |path: &Path, recursive| {
        let (completed, log) = fixture.run_sftp(
            json!({ "kind": "delete", "path": path, "confirmed_path": path, "recursive": recursive }),
            fixture.trust.clone(),
            false,
        );
        assert!(
            log.contains("Accepted publickey") && log.contains("subsystem"),
            "{log}"
        );
        assert!(!log.contains("Starting session: command"), "{log}");
        completed
    };

    let file = root.join("stale file");
    fs::write(&file, b"stale").unwrap();
    assert_eq!(delete(&file, false).outcome, RunOutcome::Success);
    assert!(!file.exists());
    let empty = root.join("empty");
    fs::create_dir(&empty).unwrap();
    assert_eq!(delete(&empty, false).outcome, RunOutcome::Success);
    assert!(!empty.exists());

    let tree = root.join("cleanup");
    let nested = tree.join("subdir");
    let child = nested.join("child");
    let outside = root.join("outside");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"keep").unwrap();
    fs::write(&child, b"old").unwrap();
    let failed = RunOutcome::Failed { exit_code: None };
    assert_eq!(delete(&tree, false).outcome, failed);
    assert!(child.is_file());

    let alias = tree.join("link outside");
    symlink(&outside, &alias).unwrap();
    assert_eq!(delete(&tree, true).outcome, failed);
    assert_eq!(delete(&alias, true).outcome, failed);
    assert_eq!(delete(&alias.join("valuable"), false).outcome, failed);
    assert!(child.is_file());
    assert!(alias.is_symlink());
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"keep");
    fs::remove_file(&alias).unwrap();

    let unsafe_name = tree.join("unsafe\\child");
    fs::write(&unsafe_name, b"keep until manually renamed").unwrap();
    assert_eq!(delete(&tree, true).outcome, RunOutcome::Blocked);
    assert!(child.is_file());
    assert!(unsafe_name.is_file());
    fs::remove_file(&unsafe_name).unwrap();

    let completed = delete(&tree, true);
    assert_eq!(
        completed.outcome,
        RunOutcome::Success,
        "{:?}",
        completed.detail
    );
    assert_eq!(
        completed.detail.as_deref(),
        Some("Deleted 3 remote entries via SFTP")
    );
    assert!(!tree.exists());
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"keep");

    let oversized = root.join("oversized");
    fs::create_dir(&oversized).unwrap();
    for index in 0..4096 {
        fs::File::create(oversized.join(index.to_string())).unwrap();
    }
    let blocked = delete(&oversized, true);
    assert_eq!(blocked.outcome, RunOutcome::Blocked);
    assert!(blocked.detail.unwrap().contains("budget"));
    assert_eq!(fs::read_dir(&oversized).unwrap().count(), 4096);
}

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_sftp_reports_partial_deletion_if_the_server_refuses_rmdir() {
    let fixture = Sshd::new();
    let root = &fixture.profile.0;
    let directory = root.join("partial");
    let child = directory.join("child");
    fs::create_dir(&directory).unwrap();
    fs::write(&child, b"temporary fixture data").unwrap();
    let config = fs::read_to_string(&fixture.config).unwrap();
    fs::write(
        &fixture.config,
        config.replace(
            "Subsystem sftp internal-sftp",
            "Subsystem sftp internal-sftp -P rmdir",
        ),
    )
    .unwrap();
    let (completion, log) = fixture.run_sftp(
        json!({ "kind": "delete", "path": directory, "confirmed_path": directory, "recursive": true }),
        fixture.trust.clone(),
        false,
    );
    assert_eq!(
        completion.outcome,
        RunOutcome::Failed { exit_code: None },
        "{log}"
    );
    assert_eq!(
        completion.detail.as_deref(),
        Some("SFTP deletion stopped after removing part of the remote tree")
    );
    assert!(!child.exists());
    assert!(directory.is_dir());
}
