use super::*;

#[test]
fn helper_command_is_fixed_and_fits_both_ssh_routes() {
    let passwordless = command(None).unwrap();
    assert!(
        passwordless.starts_with("exec /usr/bin/sudo -k -n -u root -- /usr/bin/python3 -I -S -c '")
    );
    let password = command(Some("[kortty-sudo-fixture] ")).unwrap();
    assert!(password.starts_with(
        "exec /usr/bin/sudo -k -S -p '[kortty-sudo-fixture] ' -u root -- /usr/bin/python3 -I -S -c '"
    ));
    assert!(password.len() < kortty_ssh::MAX_EXEC_COMMAND_BYTES);
    let transfer = password_file::transfer_command(Some("[kortty-sudo-fixture] ")).unwrap();
    assert_eq!(transfer, format!("{password} transfer"));
    assert!(password_file::transfer_command(Some(&password::marker([0xff; 16]))).is_ok());
    assert!(valid_path("/etc/it's a literal $(expression).txt"));
    for path in [
        "etc/file",
        "/",
        "//etc/file",
        "/etc/../file",
        "/./file",
        "/bad\nname",
    ] {
        assert!(!valid_path(path));
    }
    assert_eq!(
        hash(b"external").unwrap(),
        "3c4623849a49a53911c4a3e48d8cead8a1858960bccdea7a1b978d73ec2f06d7"
    );
}

#[test]
fn typed_file_actions_validate_paths_modes_and_the_request_bound() {
    assert!(ProtectedOperation::Mkdir.valid_for("/approved/folder"));
    assert!(!ProtectedOperation::Chmod { mode: 0o4600 }.valid_for("/approved/file"));
    assert!(
        !ProtectedOperation::Chown {
            uid: None,
            gid: None
        }
        .valid_for("/approved/file")
    );
    assert!(
        !ProtectedOperation::Chown {
            uid: Some(u32::MAX),
            gid: Some(0)
        }
        .valid_for("/approved/file")
    );
    assert!(
        !ProtectedOperation::Copy {
            to: "/approved/source".into()
        }
        .valid_for("/approved/source")
    );
    assert!(
        !ProtectedOperation::Copy {
            to: "/approved/../elsewhere".into()
        }
        .valid_for("/approved/source")
    );
    let directory = format!("/{}", format!("{}/", "\"".repeat(255)).repeat(7));
    let from = format!("{directory}{}", "\"".repeat(250));
    let to = format!("{directory}{}", "\"".repeat(249));
    assert!(valid_path(&from) && valid_path(&to));
    assert!(!ProtectedOperation::Copy { to }.valid_for(&from));
    assert!(
        ProtectedOperation::Copy {
            to: "/approved/destination".into()
        }
        .valid_for("/approved/source")
    );
    for path in ["/approved/source", "/approved/../elsewhere", "relative"] {
        assert!(!ProtectedOperation::Rename { to: path.into() }.valid_for("/approved/source"));
    }
    assert!(
        ProtectedOperation::Rename {
            to: "/another/parent/destination".into()
        }
        .valid_for("/approved/source")
    );
    assert!(!ProtectedOperation::Delete { recursive: true }.valid_for("/"));
    assert!(ProtectedOperation::Delete { recursive: false }.valid_for("/approved/source"));
    assert!(
        !ProtectedOperation::CopyTree {
            to: "/approved/source/nested".into()
        }
        .valid_for("/approved/source")
    );
    assert!(
        ProtectedOperation::CopyTree {
            to: "/approved/source-copy".into()
        }
        .valid_for("/approved/source")
    );
}

#[test]
fn response_framing_rejects_bad_lengths_shapes_and_unbounded_text() {
    let digest = "a".repeat(64);
    let encoded = serde_json::json!({
        "status": "read", "size": 4, "sha256": digest, "version": digest,
    });
    let mut bytes = Vec::new();
    encode(&mut bytes, encoded.clone());
    assert!(matches!(decode(&mut bytes.as_slice()), Ok(Response::Read(body)) if body.valid()));
    let mut invalid = encoded.clone();
    invalid["size"] = (MAX_EDIT_BYTES + 1).into();
    bytes.clear();
    encode(&mut bytes, invalid);
    assert!(matches!(decode(&mut bytes.as_slice()), Ok(Response::Read(body)) if !body.valid()));
    let mut invalid = encoded;
    invalid["path"] = "/unexpected".into();
    bytes.clear();
    encode(&mut bytes, invalid);
    assert!(decode(&mut bytes.as_slice()).is_err());
    assert!(decode(&mut ((MAX_HEADER + 1) as u32).to_be_bytes().as_slice()).is_err());
    assert!(decode(&mut &b"\x00\x00\x00\x04{}"[..]).is_err());
    assert!(!valid_text("nul\0byte"));
}

#[test]
fn sudo_prompt_requires_the_exact_nonce_at_the_start_of_a_line() {
    let nonce = "[kortty-sudo-fixture] ";
    let mut prompt = Prompt::new(nonce.into());
    assert_eq!(prompt.observe(b"echo '[kortty-sudo-fixture] '\n"), 0);
    assert_eq!(prompt.observe(b"[kortty-sudo-wrong] \n"), 0);
    for (index, chunk) in nonce.as_bytes().chunks(3).enumerate() {
        assert_eq!(
            prompt.observe(chunk),
            usize::from((index + 1) * 3 >= nonce.len())
        );
    }
    assert_eq!(
        prompt.observe(b"\n[kortty-sudo-fixture] \n[kortty-sudo-fixture] "),
        2
    );
}

fn encode(output: &mut impl Write, value: serde_json::Value) {
    let value = serde_json::to_vec(&value).unwrap();
    output
        .write_all(&(value.len() as u32).to_be_bytes())
        .unwrap();
    output.write_all(&value).unwrap();
    output.flush().unwrap();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{self, DirBuilder, FileTimes};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    use kortty::ssh_runtime::password_file::HELPER;
    use serde_json::json;

    use super::*;

    struct Helper {
        child: Child,
        input: Option<ChildStdin>,
        output: ChildStdout,
    }

    impl Helper {
        fn start() -> Self {
            Self::start_with(&[])
        }

        fn start_with(args: &[&str]) -> Self {
            let mut child = Command::new("/usr/bin/python3")
                .args(["-I", "-S", "-c", HELPER])
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let input = child.stdin.take();
            let output = child.stdout.take().unwrap();
            let mut helper = Self {
                child,
                input,
                output,
            };
            assert!(matches!(helper.response(), Response::Hello { protocol: 1 }));
            helper
        }

        fn request(&mut self, request: serde_json::Value) {
            encode(self.input.as_mut().unwrap(), request);
        }

        fn response(&mut self) -> Response {
            decode(&mut self.output).unwrap()
        }

        fn body(&mut self, body: &[u8]) {
            let input = self.input.as_mut().unwrap();
            input.write_all(body).unwrap();
            input.flush().unwrap();
        }

        fn finish(&mut self) {
            self.input = None;
            assert!(self.child.wait().unwrap().success());
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::var_os("KORTTY_PRIVILEGED_EDITOR_FIXTURE_ROOT")
                .map(PathBuf::from)
                .expect("supply an owned disposable root-controlled scratch directory");
            let root = root.canonicalize().unwrap();
            assert_ne!(root, Path::new("/"));
            let path = root.join(format!(
                "kortty-fixture-{}",
                crate::private_files::nonce().unwrap()
            ));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for name in [
                "file ' $(literal).txt",
                "link",
                "hard-link",
                "binary",
                "copy",
                "moved",
            ] {
                let _ = fs::remove_file(self.0.join(name));
            }
            let _ = fs::remove_dir(self.0.join("created"));
            let _ = fs::remove_dir_all(self.0.join("tree"));
            let _ = fs::remove_dir_all(self.0.join("tree-copy"));
            let _ = fs::remove_dir(&self.0);
        }
    }

    fn read(path: &Path) -> Body {
        let mut helper = Helper::start();
        helper.request(json!({"operation": "read", "path": path}));
        let Response::Read(body) = helper.response() else {
            panic!("Expected a checked document from the real helper");
        };
        let mut content = vec![0; body.size];
        helper.output.read_exact(&mut content).unwrap();
        assert_eq!(hash(&content).unwrap(), body.sha256);
        helper.finish();
        body
    }

    fn write(path: &Path, baseline: &Body, body: &[u8]) -> Helper {
        let mut helper = Helper::start();
        helper.request(json!({
            "operation": "write",
            "path": path,
            "size": body.len(),
            "sha256": hash(body).unwrap(),
            "version": baseline.version,
        }));
        helper
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_file_action_helper_checks_paths_and_gates_mutations() {
        let fixture = Fixture::new();
        let source = fixture.0.join("binary");
        let destination = fixture.0.join("copy");
        let directory = fixture.0.join("created");
        let bytes = b"protected raw bytes\0\xff";
        fs::write(&source, bytes).unwrap();
        let start = |path: &Path, action| {
            let mut helper = Helper::start();
            helper.request(json!({"operation": "file_action", "path": path, "action": action}));
            helper
        };
        let token = |helper: &mut Helper| match helper.response() {
            Response::Ready { token } => token,
            _ => panic!("expected the helper to wait for commit"),
        };
        let finish = |mut helper: Helper| {
            let ready = token(&mut helper);
            helper.body(ready.as_bytes());
            assert!(matches!(helper.response(), Response::FileActionApplied));
            helper.finish();
        };
        let mut cancelled = start(&directory, json!({"kind": "mkdir"}));
        token(&mut cancelled);
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        assert!(!directory.exists());
        finish(start(&directory, json!({"kind": "mkdir"})));
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        finish(start(&source, json!({"kind": "chmod", "mode": 0o640})));
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o640
        );
        finish(start(
            &source,
            json!({"kind": "chown", "uid": null, "gid": 42}),
        ));
        assert_eq!(fs::metadata(&source).unwrap().gid(), 42);

        let copy = || start(&source, json!({"kind": "copy", "to": destination}));
        let mut occupied = copy();
        let ready = token(&mut occupied);
        fs::write(&destination, b"existing").unwrap();
        occupied.body(ready.as_bytes());
        assert!(matches!(occupied.response(), Response::Conflict));
        occupied.finish();
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        fs::remove_file(&destination).unwrap();

        let mut stale = copy();
        let ready = token(&mut stale);
        let moved = fixture.0.join("moved");
        fs::rename(&source, &moved).unwrap();
        fs::write(&source, b"replacement").unwrap();
        stale.body(ready.as_bytes());
        assert!(matches!(stale.response(), Response::Conflict));
        stale.finish();
        assert!(!destination.exists());
        fs::remove_file(&source).unwrap();
        fs::rename(moved, &source).unwrap();

        let link = fixture.0.join("link");
        symlink(&source, &link).unwrap();
        let mut linked = start(&link, json!({"kind": "chmod", "mode": 0o600}));
        assert!(matches!(linked.response(), Response::Blocked));
        linked.finish();

        finish(copy());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 4);
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_rename_and_delete_recheck_protected_paths_at_commit() {
        let fixture = Fixture::new();
        let tree = fixture.0.join("tree");
        let source = fixture.0.join("binary");
        let destination = tree.join("renamed");
        fs::create_dir(&tree).unwrap();
        fs::write(&source, b"protected").unwrap();
        let start = |path: &Path, action| {
            let mut helper = Helper::start();
            helper.request(json!({"operation": "file_action", "path": path, "action": action}));
            helper
        };
        let ready = |helper: &mut Helper| match helper.response() {
            Response::Ready { token } => token,
            _ => panic!("expected the helper to wait for commit"),
        };
        let rename = || start(&source, json!({"kind": "rename", "to": destination}));
        let delete = |recursive| start(&tree, json!({"kind": "delete", "recursive": recursive}));

        let mut occupied = rename();
        let token = ready(&mut occupied);
        fs::write(&destination, b"existing").unwrap();
        occupied.body(token.as_bytes());
        assert!(matches!(occupied.response(), Response::Conflict));
        occupied.finish();
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert_eq!(fs::read(&source).unwrap(), b"protected");
        fs::remove_file(&destination).unwrap();

        let mut rename = rename();
        let token = ready(&mut rename);
        rename.body(token.as_bytes());
        assert!(matches!(rename.response(), Response::FileActionApplied));
        rename.finish();
        assert_eq!(fs::read(&destination).unwrap(), b"protected");
        assert!(!source.exists());

        let mut nonrecursive = delete(false);
        assert!(matches!(nonrecursive.response(), Response::Blocked));
        nonrecursive.finish();
        let link = tree.join("link");
        symlink(&destination, &link).unwrap();
        let mut linked = delete(true);
        assert!(matches!(linked.response(), Response::Blocked));
        linked.finish();
        fs::remove_file(link).unwrap();

        let mut cancelled = delete(true);
        ready(&mut cancelled);
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        assert_eq!(fs::read(&destination).unwrap(), b"protected");

        let mut stale = delete(true);
        let token = ready(&mut stale);
        fs::write(&destination, b"changed").unwrap();
        stale.body(token.as_bytes());
        assert!(matches!(stale.response(), Response::Conflict));
        stale.finish();
        assert_eq!(fs::read(&destination).unwrap(), b"changed");

        let mut committed = delete(true);
        let token = ready(&mut committed);
        committed.body(token.as_bytes());
        assert!(matches!(committed.response(), Response::FileActionApplied));
        committed.finish();
        assert!(!tree.exists());
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_tree_copy_checks_scope_and_publishes_without_replacing() {
        let fixture = Fixture::new();
        let source = fixture.0.join("tree");
        let target = fixture.0.join("tree-copy");
        let nested = source.join("nested");
        let file = nested.join("data");
        fs::create_dir_all(nested.join("empty")).unwrap();
        fs::write(&file, b"root-owned\0data").unwrap();
        let copy = |destination: &Path| {
            let mut helper = Helper::start();
            helper.request(json!({
                "operation": "file_action", "path": source,
                "action": {"kind": "copy_tree", "to": destination},
            }));
            helper
        };
        let ready = |helper: &mut Helper| match helper.response() {
            Response::Ready { token } => token,
            _ => panic!("expected the checked tree to be staged"),
        };
        let mut descendant = copy(&nested.join("copy"));
        assert!(matches!(descendant.response(), Response::Blocked));
        descendant.finish();

        let link = source.join("link");
        symlink(&file, &link).unwrap();
        let mut linked = copy(&target);
        assert!(matches!(linked.response(), Response::Blocked));
        linked.finish();
        fs::remove_file(link).unwrap();

        let mut cancelled = copy(&target);
        ready(&mut cancelled);
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        assert!(!target.exists());
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);

        let mut stale = copy(&target);
        let token = ready(&mut stale);
        fs::write(&file, b"changed contents").unwrap();
        stale.body(token.as_bytes());
        assert!(matches!(stale.response(), Response::Conflict));
        stale.finish();
        assert!(!target.exists());

        let mut occupied = copy(&target);
        let token = ready(&mut occupied);
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"existing").unwrap();
        occupied.body(token.as_bytes());
        assert!(matches!(occupied.response(), Response::Conflict));
        occupied.finish();
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"existing");
        fs::remove_file(target.join("keep")).unwrap();
        fs::remove_dir(&target).unwrap();

        let acl = [
            (1_u16, 7_u16, u32::MAX),
            (2, 7, 0),
            (4, 0, u32::MAX),
            (16, 7, u32::MAX),
            (32, 0, u32::MAX),
        ];
        let mut encoded = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, uid) in acl {
            encoded.extend(tag.to_le_bytes());
            encoded.extend(permissions.to_le_bytes());
            encoded.extend(uid.to_le_bytes());
        }
        rustix::fs::setxattr(
            &fixture.0,
            "system.posix_acl_default",
            &encoded,
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        let mut committed = copy(&target);
        let token = ready(&mut committed);
        committed.body(token.as_bytes());
        assert!(matches!(committed.response(), Response::FileActionApplied));
        committed.finish();
        assert_eq!(
            fs::read(target.join("nested/data")).unwrap(),
            b"changed contents"
        );
        assert!(target.join("nested/empty").is_dir());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            fs::metadata(target.join("nested")).unwrap().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(target.join("nested/data")).unwrap().mode() & 0o7777,
            0o600
        );
        for path in [&target, &target.join("nested"), &target.join("nested/data")] {
            for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
                assert_eq!(
                    rustix::fs::getxattr_query(path, attribute),
                    Err(rustix::io::Errno::NODATA)
                );
            }
        }
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_helper_preserves_mode_and_requires_exact_baseline_and_commit() {
        let fixture = Fixture::new();
        let target = fixture.0.join("file ' $(literal).txt");
        fs::write(&target, b"before").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let attribute = "user.kortty-fixture";
        let value = b"retained attributes";
        rustix::fs::setxattr(
            target.as_path(),
            attribute,
            value,
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        let baseline = read(&target);
        let mut uncommitted = write(&target, &baseline, b"uncommitted");
        assert!(matches!(uncommitted.response(), Response::Accept));
        uncommitted.body(b"uncommitted");
        assert!(matches!(uncommitted.response(), Response::Ready { .. }));
        uncommitted.input = None;
        assert!(matches!(uncommitted.response(), Response::Failed));
        uncommitted.finish();
        assert_eq!(fs::read(&target).unwrap(), b"before");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);

        let mut committed = write(&target, &baseline, b"after!");
        assert!(matches!(committed.response(), Response::Accept));
        committed.body(b"after!");
        let Response::Ready { token } = committed.response() else {
            panic!("Expected a commit token");
        };
        committed.body(token.as_bytes());
        let Response::Written(saved) = committed.response() else {
            panic!("Expected a publication receipt");
        };
        committed.finish();
        assert_eq!(fs::read(&target).unwrap(), b"after!");
        assert_eq!(
            target.metadata().unwrap().permissions().mode() & 0o777,
            0o640
        );
        let mut retained = [0; 64];
        let length = rustix::fs::getxattr(target.as_path(), attribute, &mut retained[..]).unwrap();
        assert_eq!(&retained[..length], value);
        assert_eq!(read(&target).version, saved.version);
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);

        let timestamp = target.metadata().unwrap().modified().unwrap();
        fs::write(&target, b"change").unwrap();
        fs::File::open(&target)
            .unwrap()
            .set_times(FileTimes::new().set_modified(timestamp))
            .unwrap();
        let mut stale = write(&target, &saved, b"stale!");
        assert!(matches!(stale.response(), Response::Conflict));
        stale.finish();
        assert_eq!(fs::read(&target).unwrap(), b"change");

        let link = fixture.0.join("link");
        symlink(&target, &link).unwrap();
        let mut linked = Helper::start();
        linked.request(json!({"operation": "read", "path": link}));
        assert!(matches!(linked.response(), Response::Blocked));
        linked.finish();

        let hard_link = fixture.0.join("hard-link");
        fs::hard_link(&target, &hard_link).unwrap();
        let mut multiply_linked = Helper::start();
        multiply_linked.request(json!({"operation": "read", "path": target}));
        assert!(matches!(multiply_linked.response(), Response::Blocked));
        multiply_linked.finish();
        fs::remove_file(hard_link).unwrap();

        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o770)).unwrap();
        let mut writable = Helper::start();
        writable.request(json!({"operation": "read", "path": target}));
        assert!(matches!(writable.response(), Response::Blocked));
        writable.finish();
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_tree_download_helper_reviews_metadata_then_gates_exact_bodies() {
        let fixture = Fixture::new();
        let root = fixture.0.join("source");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::create_dir(root.join("nested/empty")).unwrap();
        let path = root.join("nested/data");
        let bytes = b"private reviewed bytes\0";
        fs::write(&path, bytes).unwrap();
        let reviewed = |operation| {
            let mut helper = Helper::start_with(&["transfer"]);
            helper.request(json!({"operation": operation, "path": root}));
            let Response::TreeManifest {
                witness,
                entries: 3,
                size,
            } = helper.response()
            else {
                panic!("expected a bounded remote source manifest");
            };
            assert_eq!(size, bytes.len() as u64);
            assert!(
                matches!(helper.response(), Response::TreeDirectory { path } if path == "nested")
            );
            assert!(
                matches!(helper.response(), Response::TreeFile { path, size, sha256 }
                if path == "nested/data" && size == bytes.len() as u64 && sha256 == hash(bytes).unwrap())
            );
            assert!(
                matches!(helper.response(), Response::TreeDirectory { path } if path == "nested/empty")
            );
            (helper, witness)
        };
        let (mut inspect, approved) = reviewed("inspect_tree_source");
        inspect.finish();
        let (mut cancelled, witness) = reviewed("download_tree");
        assert_eq!(witness, approved);
        assert!(matches!(cancelled.response(), Response::Ready { .. }));
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        let (mut download, witness) = reviewed("download_tree");
        let Response::Ready { token } = download.response() else {
            panic!("expected a challenge before file disclosure");
        };
        download.body(token.as_bytes());
        let mut received = vec![0; bytes.len()];
        download.output.read_exact(&mut received).unwrap();
        assert_eq!(received, bytes);
        assert!(
            matches!(download.response(), Response::TreeRead { witness: final_witness } if final_witness == witness)
        );
        download.finish();

        let (mut stale, _) = reviewed("download_tree");
        let Response::Ready { token } = stale.response() else {
            panic!("expected disclosure challenge");
        };
        fs::write(&path, b"concurrent replacement").unwrap();
        stale.body(token.as_bytes());
        assert!(matches!(stale.response(), Response::Conflict));
        stale.finish();
        symlink(&path, root.join("link")).unwrap();
        let mut unsafe_source = Helper::start_with(&["transfer"]);
        unsafe_source.request(json!({"operation": "inspect_tree_source", "path": root}));
        assert!(matches!(unsafe_source.response(), Response::Blocked));
        unsafe_source.finish();
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_tree_helper_publishes_complete_tree_and_preserves_late_destination() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o2700)).unwrap();
        assert_eq!(
            fixture.0.metadata().unwrap().permissions().mode() & 0o7777,
            0o2700,
        );
        let target = fixture.0.join("reviewed");
        let mut inspect = Helper::start_with(&["transfer"]);
        inspect.request(json!({"operation": "inspect_tree", "path": target}));
        let Response::TreeTarget {
            witness,
            token: None,
        } = inspect.response()
        else {
            panic!("expected a read-only witness for an absent target");
        };
        inspect.finish();
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());

        let bytes = b"reviewed tree payload\0";
        let request = json!({
            "operation": "create_tree", "path": target, "entries": 3, "size": bytes.len(),
        });
        let ready = || {
            let mut helper = Helper::start_with(&["transfer"]);
            helper.request(request.clone());
            let Response::TreeTarget {
                witness: current,
                token: Some(token),
            } = helper.response()
            else {
                panic!("expected a tree staging challenge");
            };
            assert_eq!(current, witness);
            assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
            helper.body(token.as_bytes());
            assert!(matches!(helper.response(), Response::Accept));
            helper.request(json!({"kind": "folder", "path": "nested"}));
            helper.request(json!({"kind": "folder", "path": "nested/empty"}));
            helper.request(json!({
                "kind": "file", "path": "nested/file.bin", "size": bytes.len(),
                "sha256": hash(bytes).unwrap(),
            }));
            helper.body(bytes);
            let Response::Ready { token } = helper.response() else {
                panic!("expected private, synchronized staging before commit");
            };
            assert!(!target.exists());
            (helper, token)
        };

        let (mut cancelled, _) = ready();
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());

        let (mut raced, token) = ready();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();
        raced.body(token.as_bytes());
        assert!(matches!(raced.response(), Response::Conflict));
        raced.finish();
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
        fs::remove_file(target.join("sentinel")).unwrap();
        fs::remove_dir(&target).unwrap();

        let (mut committed, token) = ready();
        committed.body(token.as_bytes());
        assert!(
            matches!(committed.response(), Response::TreeCreated { entries: 3, size }
            if size == bytes.len() as u64)
        );
        committed.finish();
        assert_eq!(fs::read(target.join("nested/file.bin")).unwrap(), bytes);
        assert!(target.join("nested/empty").is_dir());
        assert_eq!(
            target.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }

    #[test]
    #[ignore = "requires explicit root-controlled Linux scratch and Python 3, no sudo is invoked"]
    fn privileged_transfer_helper_requires_exact_binary_body_and_no_replace_commit() {
        use std::os::unix::fs::MetadataExt;

        let fixture = Fixture::new();
        let target = fixture.0.join("binary");
        let bytes = (0..196_610)
            .map(|index| (index % 256) as u8)
            .collect::<Vec<_>>();
        let digest = hash(&bytes).unwrap();
        let request = json!({
            "operation": "create", "path": target, "size": bytes.len(), "sha256": digest,
        });
        let mut wrong_mode = Helper::start();
        wrong_mode.request(request.clone());
        assert!(matches!(wrong_mode.response(), Response::Blocked));
        wrong_mode.finish();

        let mut oversized = Helper::start_with(&["transfer"]);
        let mut changed = request.clone();
        changed["size"] = json!(MAX_TRANSFER_BYTES + 1);
        oversized.request(changed);
        assert!(matches!(oversized.response(), Response::Blocked));
        oversized.finish();

        let mut bad_hash = Helper::start_with(&["transfer"]);
        let mut changed = request.clone();
        changed["sha256"] = json!("0".repeat(64));
        bad_hash.request(changed);
        assert!(matches!(bad_hash.response(), Response::Accept));
        bad_hash.body(&bytes);
        assert!(matches!(bad_hash.response(), Response::Failed));
        bad_hash.finish();
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());

        let ready = || {
            let mut helper = Helper::start_with(&["transfer"]);
            helper.request(request.clone());
            assert!(matches!(helper.response(), Response::Accept));
            helper.body(&bytes);
            let Response::Ready { token } = helper.response() else {
                panic!("Expected a private binary stage");
            };
            (helper, token)
        };
        let (mut cancelled, _) = ready();
        cancelled.input = None;
        assert!(matches!(cancelled.response(), Response::Failed));
        cancelled.finish();
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());

        let (mut raced, token) = ready();
        fs::write(&target, b"concurrent destination").unwrap();
        raced.body(token.as_bytes());
        assert!(matches!(raced.response(), Response::Conflict));
        raced.finish();
        assert_eq!(fs::read(&target).unwrap(), b"concurrent destination");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
        fs::remove_file(&target).unwrap();

        let (mut create, token) = ready();
        create.body(token.as_bytes());
        assert!(
            matches!(create.response(), Response::Created { size, sha256 }
            if size == bytes.len() as u64 && sha256 == digest)
        );
        create.finish();
        let info = target.metadata().unwrap();
        assert_eq!(info.permissions().mode() & 0o777, 0o600);
        assert_eq!(info.nlink(), 1);

        let mut download = Helper::start_with(&["transfer"]);
        download.request(json!({"operation": "download", "path": target}));
        assert!(
            matches!(download.response(), Response::Download { size } if size == bytes.len() as u64)
        );
        let mut received = vec![0; bytes.len()];
        download.output.read_exact(&mut received).unwrap();
        assert_eq!(received, bytes);
        assert!(
            matches!(download.response(), Response::Downloaded { size, sha256 }
            if size == received.len() as u64 && sha256 == digest)
        );
        download.finish();
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }
}
