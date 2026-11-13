use std::ffi::OsStr;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::private_files::{Directory, nonce};

use super::process::{Process, executable};
use super::*;

#[test]
fn git_urls_and_branch_refs_reject_transport_and_argument_injection() {
    assert_eq!(
        GitUrl::parse(" HTTPS://EXAMPLE.ORG/team/repo.git ")
            .unwrap()
            .as_str(),
        "https://example.org/team/repo.git"
    );
    for (input, expected) in [
        (
            "ssh://git@GIT.EXAMPLE.ORG:22/team.git",
            "ssh://git@git.example.org/team.git",
        ),
        (
            "git://GIT.EXAMPLE.ORG:9418/team.git",
            "git://git.example.org/team.git",
        ),
    ] {
        assert_eq!(GitUrl::parse(input).unwrap().as_str(), expected);
    }
    for valid in [
        "http://git.example.org/team.git",
        "git://127.0.0.1:19418/team.git",
        "ssh://git@[::1]:2222/team.git",
    ] {
        assert!(GitUrl::parse(valid).is_ok(), "{valid}");
    }
    for rejected in [
        "-u",
        "file:///tmp/remote",
        "/tmp/remote",
        "git@host:repo.git",
        "ext::sh -c command",
        "https:///host/repo",
        "https://host",
        "https://host:0/repo",
        "https://host/repo?token=private",
        "https://host/repo#private",
        "https://login:secret@host/repo",
        "https://@host/repo",
        "ssh://git:secret@host/repo",
        "ssh://-option@host/repo",
        "ssh://-option/repo",
        "ssh://%2doption/repo",
        "ssh://host/repo%0d%0a",
        "ssh://host/repo%00",
        "ssh://host/repo\u{202e}",
    ] {
        assert!(GitUrl::parse(rejected).is_err(), "{rejected}");
    }
    assert!(Branch::parse("refs/heads/team/production").is_ok());
    for rejected in [
        "HEAD",
        "refs/tags/main",
        "refs/heads/",
        "refs/heads/team//main",
        "refs/heads/team/.main",
        "refs/heads/team/main.lock",
        "refs/heads/team/main:refs/evil",
        "refs/heads/team/main^",
        "refs/heads/team/main@{1}",
    ] {
        assert!(Branch::parse(rejected).is_err(), "{rejected}");
    }
}

#[test]
fn tree_records_require_a_regular_root_blob_and_respect_priority_and_limits() {
    let hash = "a".repeat(40);
    let file = format!("100644 blob {hash}    12\t{LEGACY}\0");
    let primary = format!("100755 blob {hash}    21\t{PRIMARY}\0");
    let linked = format!("120000 blob {hash}     2\t{PRIMARY}\0");
    let submodule = format!("160000 commit {hash}     -\t{PRIMARY}\0");
    let oversized = format!(
        "100644 blob {hash}  {}\t{PRIMARY}\0",
        MAX_TEAMWORK_BYTES + 1
    );
    assert!(connection_blob(b"", 40).unwrap().is_none());
    assert_eq!(
        connection_blob(file.as_bytes(), 40).unwrap().unwrap().size,
        12
    );
    assert_eq!(
        connection_blob(format!("{file}{primary}").as_bytes(), 40)
            .unwrap()
            .unwrap()
            .size,
        21
    );
    for bad in [linked, submodule, oversized, format!("{primary}{primary}")] {
        assert!(connection_blob(format!("{file}{bad}").as_bytes(), 40).is_err());
    }
}

#[test]
#[ignore = "requires system Git and git-daemon on loopback"]
fn git_reads_the_tracked_branch_over_loopback_after_a_default_change_and_force_update() {
    let executable = executable().expect("system Git for the Teamwork integration test");
    let mut repository = Repository::new(executable);
    repository.git(&["init", "--quiet", "--initial-branch=main", "--template="]);
    repository.write(PRIMARY, "primary");
    repository.write(LEGACY, "legacy");
    repository.commit();
    let original = repository.git(&["rev-parse", "HEAD"]);
    let url = repository.serve();
    let cancel = AtomicBool::new(false);

    let (tracked, entries) = read(&url, None, &cancel).unwrap();
    assert_eq!(tracked.as_str(), "refs/heads/main");
    assert_eq!(entries[0].id, "primary");
    assert_eq!(entries[0].connection.id, 0);

    repository.git(&["rm", "--quiet", "--", PRIMARY]);
    repository.commit();
    repository.git(&["branch", "other", &original]);
    repository.git(&["symbolic-ref", "HEAD", "refs/heads/other"]);

    let (same, entries) = read(&url, Some(&tracked), &cancel).unwrap();
    assert_eq!(same, tracked);
    assert_eq!(entries[0].id, "legacy");
    let (different, entries) = read(&url, None, &cancel).unwrap();
    assert_eq!(different.as_str(), "refs/heads/other");
    assert_eq!(entries[0].id, "primary");

    repository.git(&["update-ref", "refs/heads/main", &original]);
    assert_eq!(
        read(&url, Some(&tracked), &cancel).unwrap().1[0].id,
        "primary"
    );

    repository.git(&["rm", "--force", "--quiet", "--", LEGACY]);
    repository.commit();
    assert!(read(&url, Some(&different), &cancel).unwrap().1.is_empty());
}

#[test]
fn cancelled_git_read_stops_before_resolving_git_or_connecting() {
    let url = GitUrl::parse("git://127.0.0.1:1/source").unwrap();
    assert_eq!(
        read(&url, None, &AtomicBool::new(true)).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

struct Repository {
    root: PathBuf,
    executable: PathBuf,
    daemon: Option<Process>,
}

impl Repository {
    fn new(executable: PathBuf) -> Self {
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let name = format!("kortty-teamwork-git-test-{}", nonce().unwrap());
        Directory::open(&parent, false)
            .unwrap()
            .create_private_subdirectory(OsStr::new(&name))
            .unwrap();
        let root = parent.join(name);
        fs::create_dir(root.join("source")).unwrap();
        Self {
            root,
            executable,
            daemon: None,
        }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.root)
            .env("GIT_AUTHOR_NAME", "Teamwork Test")
            .env("GIT_AUTHOR_EMAIL", "teamwork@example.org")
            .env("GIT_COMMITTER_NAME", "Teamwork Test")
            .env("GIT_COMMITTER_EMAIL", "teamwork@example.org")
            .current_dir(cwd)
            .stdin(Stdio::null());
        for key in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = self
            .command(&self.root.join("source"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn commit(&self) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "-m", "Shared connections"]);
    }

    fn write(&self, path: &str, id: &str) {
        fs::write(
            self.root.join("source").join(path),
            format!("<connections><connection id=\"{id}\"><host>ssh.example.org</host><username>ops</username></connection></connections>"),
        )
        .unwrap();
    }

    fn serve(&mut self) -> GitUrl {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let port = format!("--port={}", address.port());
        let mut daemon = self.command(&self.root);
        daemon
            .args([
                "daemon",
                "--reuseaddr",
                "--export-all",
                "--base-path=.",
                "--listen=127.0.0.1",
                &port,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.daemon = Some(Process::spawn(&mut daemon).unwrap());
        wait_for_daemon(self.daemon.as_mut().unwrap(), address);
        GitUrl::parse(&format!("git://{address}/source")).unwrap()
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        drop(self.daemon.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for_daemon(daemon: &mut Process, address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "Git daemon stopped"
        );
        assert!(
            Instant::now() < deadline,
            "Git daemon did not listen on loopback"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
