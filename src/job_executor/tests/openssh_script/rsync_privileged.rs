use std::sync::atomic::AtomicBool;

use kortty_jobs::{Job, JobGraph, SchedulerStore};
use kortty_server_archive::privileged_rsync::{Operation, Spec};
use kortty_server_archive::rsync_tree::DeleteScope;

use crate::job_executor::rsync;
use crate::scheduler_window::sync;

use super::*;

mod existing_file;
mod local_file_download;
mod mixed;

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync helper fixture"]
fn openssh_passwordless_rsync_copies_files_and_trees_through_protected_roots() {
    let fixture = Fixture::new();
    let local = &fixture.profile.0;
    let upload_file = local.join("source-file");
    fs::write(&upload_file, b"scheduled upload file\n").unwrap();
    let upload_tree = local.join("source-tree");
    fs::create_dir_all(upload_tree.join("nested")).unwrap();
    fs::write(
        upload_tree.join("nested/content"),
        b"scheduled upload tree\n",
    )
    .unwrap();
    let download_file = local.join("download-file");
    let download_tree = local.join("download-tree");
    let merge = local.join("merged-tree");
    fs::create_dir(&merge).unwrap();
    fs::write(merge.join("stale"), b"stale local tree").unwrap();

    let cases = [
        Case {
            local: &upload_file,
            path: "/test-rsync/write/placed-file",
            upload: true,
            directory: false,
            replace: false,
        },
        Case {
            local: &upload_tree,
            path: "/test-rsync/write/placed-tree",
            upload: true,
            directory: true,
            replace: false,
        },
        Case {
            local: &upload_tree,
            path: "/test-rsync/replace/receiver",
            upload: true,
            directory: true,
            replace: true,
        },
        Case {
            local: &download_file,
            path: "/test-rsync/read/file",
            upload: false,
            directory: false,
            replace: false,
        },
        Case {
            local: &download_tree,
            path: "/test-rsync/read/tree",
            upload: false,
            directory: true,
            replace: false,
        },
        Case {
            local: &merge,
            path: "/test-rsync/read/tree",
            upload: false,
            directory: true,
            replace: true,
        },
    ];
    let forbidden = Spec {
        path: "/test-rsync/denied/file".into(),
        operation: Operation::Download { directory: false },
    };
    assert!(fixture.preview(&forbidden).is_err());
    for (index, case) in cases.into_iter().enumerate() {
        let action = fixture.action(&case);
        if index == 0 {
            assert_eq!(
                fixture.execute(action.clone(), false).outcome,
                RunOutcome::Blocked
            );
        }
        let result = fixture.execute(action.clone(), true);
        assert_eq!(
            result.outcome,
            RunOutcome::Success,
            "{}: {result:?}",
            case.path
        );
        if case.replace {
            assert!(
                result
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("original is retained at")
            );
        }
        if index == 0 {
            assert_eq!(fixture.execute(action, true).outcome, RunOutcome::Blocked);
        }
    }
    assert_eq!(fs::read(download_file).unwrap(), b"protected rsync file\n");
    for tree in [&download_tree, &merge] {
        assert_eq!(
            fs::read(tree.join("nested/content")).unwrap(),
            b"protected rsync tree\n"
        );
        assert!(tree.join("empty").is_dir());
    }
    assert!(!merge.join("stale").exists());
    for path in ["", "nested", "empty"] {
        assert_eq!(
            fs::metadata(download_tree.join(path)).unwrap().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(download_tree.join("nested/content"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
}

struct Case<'a> {
    local: &'a Path,
    path: &'a str,
    upload: bool,
    directory: bool,
    replace: bool,
}

struct Fixture {
    connections: Store,
    reference: ConnectionRef,
    trust: TrustStore,
    profile: Profile,
    program: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let profile = Profile::new();
        let (mut connection, pins) = privileged_fixture_route();
        connection.id = 0;
        let mut connections = Store::open(profile.0.join("connections.json")).unwrap();
        let id = connections.save(connection).unwrap();
        let reference = ConnectionRef::from(connections.get(id).unwrap());
        let trust = TrustStore::from_pins(pins).unwrap();
        let program = PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_PROGRAM").unwrap())
            .canonicalize()
            .unwrap();
        assert!(PathBuf::from(std::env::var_os("KORTTY_RSYNC_TEST_BRIDGE").unwrap()).is_file());
        Self {
            connections,
            reference,
            trust,
            profile,
            program,
        }
    }

    fn preview(
        &self,
        spec: &Spec,
    ) -> Result<kortty_server_archive::privileged_rsync::Review, String> {
        let credentials = Credentials::resolve_directory_browser(
            &self.reference,
            self.connections.get(self.reference.id).unwrap(),
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .unwrap();
        rsync::preview_privileged(
            credentials,
            &self.reference,
            spec,
            &self.trust,
            Arc::new(AtomicBool::new(false)),
            || Ok(()),
        )
    }

    fn action(&self, case: &Case<'_>) -> SshCommand {
        let operation = if case.upload {
            Operation::Upload {
                directory: case.directory,
                replace: case.replace,
                delete: if case.replace && case.directory {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                },
            }
        } else {
            Operation::Download {
                directory: case.directory,
            }
        };
        let spec = Spec {
            path: case.path.into(),
            operation,
        };
        let review = self.preview(&spec).unwrap();
        let digest = rsync::program_digest(&self.program).unwrap();
        let pins = sync::fingerprints(&sync::pins(&self.reference, &self.trust).unwrap());
        let settings = json!({"include_source_name": false, "delete": case.directory});
        let mut approval = json!({ "program": self.program, "digest": digest,
            "reference": self.reference, "pins": pins, "privileged": review });
        let kind = if case.upload {
            "rsync_upload"
        } else {
            "rsync_download"
        };
        let mut remote = json!({"kind": kind, "directory": case.directory, "elevation": "passwordless",
            "program": self.program, "local": case.local, "remote": case.path });
        match (case.upload, case.directory) {
            (true, true) => {
                approval["directory_upload"] = json!({"source": case.local, "destination": case.path,
                    "inventory": rsync::upload_tree_inventory(case.local).unwrap()});
            }
            (true, false) => {
                let (bytes, digest) = rsync::upload_digest(case.local).unwrap();
                approval["upload"] = json!({"source": case.local, "destination": case.path, "bytes": bytes, "digest": digest});
            }
            (false, true) => {
                approval["directory"] = json!({"source": case.path, "destination": case.local});
                if case.replace {
                    let snapshot =
                        rsync::review_local_existing(case.local, &AtomicBool::new(false)).unwrap();
                    remote["existing_download"] = settings.clone();
                    approval["existing_download"] = json!({"destination": case.local, "settings": settings, "snapshot": snapshot});
                }
            }
            (false, false) => {
                approval["download"] = json!({"source": case.path, "destination": case.local})
            }
        }
        if case.upload && case.replace {
            remote["existing"] = settings.clone();
            approval["existing"] = json!({"destination": case.path, "settings": settings,
                "snapshot": {"inventory": review.inventory, "fingerprint": review.fingerprint}});
        }
        remote["approval"] = approval;
        serde_json::from_value::<SshCommand>(
            json!({"connection": self.reference, "remote": remote}),
        )
        .unwrap()
        .validated()
        .unwrap()
    }

    fn execute(&self, action: SshCommand, sudo_allowed: bool) -> Completion {
        self.execute_manual(action, sudo_allowed, false)
    }

    fn execute_manual(
        &self,
        action: SshCommand,
        sudo_allowed: bool,
        cancelled: bool,
    ) -> Completion {
        let path = self.profile.0.join("jobs-privileged-rsync.json");
        let graph = JobGraph::new(vec![
            Job::new(
                JobId::new("privileged-rsync").unwrap(),
                "Protected sync",
                action,
            )
            .unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut runtime = SchedulerWindow::open(path).unwrap();
        runtime.set_connections(Some(&self.connections));
        runtime.set_execution_allowed(true);
        let mut view = runtime.new_view();
        let request = runtime
            .update_view(&mut view, Message::Run(false))
            .pop()
            .unwrap();
        let saved = Credentials::resolve(
            &request.action,
            self.connections.get(self.reference.id).unwrap(),
            &ServerAccess::default(),
            |_, _, _| Err("unexpected credential"),
        )
        .map(Some);
        let mut policy = ExecutionPolicy::unrestricted(true);
        policy.sudo_allowed = sudo_allowed;
        policy.rsync = runtime.try_rsync_run_context(&request).unwrap();
        if cancelled {
            runtime.update_view(
                &mut view,
                Message::Page(crate::scheduler_window::Page::Journal),
            );
            runtime.update_view(&mut view, Message::CancelRun(request.id));
            assert!(request.cancellation.is_requested());
        }
        let result = run(&request, None, None, &policy, saved, Ok(None), || {
            Ok(self.trust.clone())
        });
        runtime.update_runtime(Message::Finished(result.clone()));
        result
    }
}
