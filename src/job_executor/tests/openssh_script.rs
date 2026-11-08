use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kortty_jobs::{RunRequest, Scheduler};
use kortty_ssh::{Endpoint, HostKey, HostKeyStatus, TrustMode, TrustStore};
use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};
use serde_json::json;

use crate::connection_store::{Auth, Connection, SavedLogin, Store};
use crate::scheduler_window::ConnectionRef;
use crate::snippets::{Input, Library, Variable};
use crate::ssh_dialog::LoginTarget;
use crate::ssh_key_catalog::{Catalog, Edit, KeyDraft, KeyInput};
use crate::vault_window::{Kind, resolve_credential};

use super::{
    BorrowedArchivePassword, Completion, Credentials, RunOutcome, SshCommand, batch, run,
    run_command, scheduled_request, thread, virtual_terminal,
};

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
            file_transfer_allowed,
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

#[test]
#[ignore = "requires installed OpenSSH sshd and ssh-keygen and a local account"]
fn openssh_scheduled_batch_runs_in_order_and_aggregates_failed_and_unpinned_targets() {
    let first = Sshd::new();
    let unpinned = Sshd::new();
    let last = Sshd::new();
    let order = first.profile.0.join("target order");
    let (_, request, store) = scheduled_batch(
        &first.profile.0,
        &[&first, &unpinned, &last],
        r#"case "${SSH_CONNECTION##* }" in
  "$2") printf '%s\n' first > "$1"; printf '%s\n' 'secret fixture output'; exit 37 ;;
  "$3") [ "$(cat "$1")" = first ] || exit 38; printf '%s\n' last >> "$1" ;;
  *) printf '%s\n' unexpected >> "$1"; exit 39 ;;
esac"#,
        vec![
            order.to_str().unwrap().into(),
            first.port.to_string(),
            last.port.to_string(),
        ],
    );
    let credentials = Credentials::resolve_batch(&request.action, &store, |_, _, _| {
        panic!("plain fixture key unexpectedly required the vault")
    });
    let daemons = [first.spawn(), unpinned.spawn(), last.spawn()];
    let trust = TrustStore::from_pins(vec![first.pin(), last.pin()]).unwrap();
    let completed = batch::run(&request, None, true, credentials, Ok(None), || Ok(trust));
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
    let credentials = Credentials::resolve_batch(&sftp.action, &store, |_, _, _| {
        panic!("plain fixture key unexpectedly required the vault")
    });
    let trust = TrustStore::from_pins(vec![first.pin(), last.pin()]).unwrap();
    let completed = batch::run(&sftp, None, false, credentials, Ok(None), || Ok(trust));
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
    let credentials = Credentials::resolve_batch(&request.action, &store, |_, _, _| {
        panic!("plain fixture key unexpectedly required the vault")
    });
    let trust = TrustStore::from_pins(vec![first.pin(), next.pin()]).unwrap();
    let daemon = first.spawn();
    let (sender, completion) = std::sync::mpsc::sync_channel(1);
    let id = request.id;
    let worker = thread::spawn(move || {
        let result = batch::run(&request, None, true, credentials, Ok(None), || Ok(trust));
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
    let completed = run(&request, None, None, true, ready, Ok(None), || Ok(pins));
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
    let completed = run(&request, None, None, true, staged, Ok(None), || {
        TrustStore::from_pins(vec![target.pin(), jump.pin()])
    });
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
        })
        .unwrap();
    let store = Store::open(connection_path).unwrap();
    let profile = store.get(id).unwrap();
    let reference = ConnectionRef::from(profile);
    let login = |action: &SshCommand| {
        Credentials::resolve(action, profile, |id, kind, endpoint| {
            resolve_credential(&vault, id, kind, endpoint)
        })
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
        true,
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
        false,
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
        false,
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
        false,
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
        let output = profile.0.join(if pty { "pty-argv" } else { "exec-argv" });
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
            "virtual_terminal": pty.then(virtual_terminal::Options::default)
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
            Ok(None),
            || Ok(trust.clone()),
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
        let actual = fs::read(output).unwrap();
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
        0o600
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
    assert_eq!(mode(&download), 0o700);
    assert_eq!(mode(&download.join("ordinary")), 0o600);
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
        let _ = finished.send(run(&request, None, None, true, Ok(None), Ok(None), || {
            Ok(trust)
        }));
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
        let result = run(&request, None, None, false, Ok(None), Ok(None), || {
            panic!("A copy to its own source tree must fail before accessing SSH trust")
        });
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
