use crate::scheduler_window::RemotePath;
use crate::scheduler_window::rsync::{
    Elevation, MixedDestination, MixedPrivilegedReview, MixedReceiver, MixedReview,
    MixedReviewTarget,
};

use super::*;

pub(super) mod recurring;

const ROOT: &str = "/test-rsync";
const REPORT: &[u8] = b"first source wins\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverMode {
    Vacant,
    Merge,
    ScopedMirror,
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync mixed fixture"]
fn openssh_passwordless_mixed_rsync_publishes_vacant_merge_and_scoped_mirror() {
    require_isolated_root();
    let fixture = Fixture::new();
    let local_inputs = fixture.profile.0.join("inputs");
    let server_inputs = Path::new(ROOT).join("read/mixed-inputs");
    for path in [&local_inputs, &server_inputs] {
        seed_inputs(path);
    }
    for upload in [true, false] {
        for (mode, name) in [
            (ReceiverMode::Vacant, "mixed-vacant"),
            (ReceiverMode::Merge, "mixed-merge"),
            (ReceiverMode::ScopedMirror, "mixed-mirror"),
        ] {
            let destination = if upload {
                let role = if mode == ReceiverMode::Vacant {
                    "write"
                } else {
                    "replace"
                };
                Path::new(ROOT).join(role).join(name)
            } else {
                fixture.profile.0.join(name)
            };
            let old = (mode != ReceiverMode::Vacant).then(|| seed_receiver(&destination));
            let inputs = if upload {
                &local_inputs
            } else {
                &server_inputs
            };
            let target = target(&fixture, upload, &destination, sources(inputs, mode), mode);
            let review = preview(&fixture, &target).unwrap();
            assert!(target.matches(&review));
            assert_eq!(review.plan.collision_count, 1);
            assert_eq!(review.plan.collisions[0].relative, "report");
            assert_eq!(review.plan.collisions[0].winner, 0);
            assert_eq!(
                review.plan.delete,
                if mode == ReceiverMode::ScopedMirror {
                    DeleteScope::Named(vec!["scope".into()])
                } else {
                    DeleteScope::Retain
                }
            );
            let handoff = handoff(&review).to_owned();
            let action = action(&fixture, &target, review);
            if mode == ReceiverMode::Vacant {
                assert_eq!(
                    fixture.execute(action.clone(), false).outcome,
                    RunOutcome::Blocked
                );
                assert!(!destination.exists());
            }
            let completion = fixture.execute(action, true);
            assert_eq!(
                completion.outcome,
                RunOutcome::Success,
                "upload={upload}, {mode:?}: {completion:?}"
            );
            assert_projection(&destination, mode);
            if !upload && mode == ReceiverMode::Vacant {
                assert_private_download(&destination);
            }
            if let Some(previous) = old {
                assert_recovered(&completion, &destination, previous);
            }
            assert_empty_handoff(&handoff);
        }
    }
}

#[test]
#[ignore = "requires the isolated native OpenSSH and passwordless rsync mixed fixture"]
fn openssh_passwordless_mixed_rsync_refuses_stale_and_unauthorized_losing_sources() {
    require_isolated_root();
    let fixture = Fixture::new();
    for upload in [true, false] {
        let (inputs, destination) = if upload {
            (
                fixture.profile.0.join("stale-inputs"),
                Path::new(ROOT).join("write/mixed-stale"),
            )
        } else {
            (
                Path::new(ROOT).join("read/mixed-stale"),
                fixture.profile.0.join("mixed-stale"),
            )
        };
        seed_inputs(&inputs);
        let mode = ReceiverMode::Vacant;
        let target = target(&fixture, upload, &destination, sources(&inputs, mode), mode);
        let review = preview(&fixture, &target).unwrap();
        let handoff = handoff(&review).to_owned();
        let action = action(&fixture, &target, review);
        // This file loses the collision. Its reviewed bytes still bind the action.
        fs::write(inputs.join("losing/report"), b"later source edited\n").unwrap();
        let completion = fixture.execute(action, true);
        assert_eq!(
            completion.outcome,
            RunOutcome::Blocked,
            "upload={upload}: {completion:?}"
        );
        assert!(!destination.exists());
        assert_empty_handoff(&handoff);
        if !upload {
            let outside = Path::new(ROOT).join("read-neighbor");
            directory(&outside);
            write(&outside.join("report"), b"unapproved losing file\n");
            let mut denied = target;
            denied.sources[3] = outside.join("report").to_str().unwrap().into();
            assert!(preview(&fixture, &denied).is_err());
            assert!(!destination.exists());
            assert_empty_handoff(&handoff);
        }
    }
}

fn require_isolated_root() {
    assert!(rustix::process::geteuid().is_root());
    assert_eq!(
        fs::read(Path::new(ROOT).join(".mixed-rsync-fixture")).unwrap(),
        b"kortty-mixed-rsync-fixture-v1\n"
    );
}

fn directory(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

fn write(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
}

fn seed_inputs(path: &Path) {
    directory(path);
    for relative in [
        "first",
        "named",
        "named/scope",
        "named/scope/empty",
        "flat",
        "losing",
    ] {
        directory(&path.join(relative));
    }
    write(&path.join("first/report"), REPORT);
    write(&path.join("named/scope/new"), b"named source\n");
    write(&path.join("flat/flattened"), b"folder contents\n");
    write(&path.join("losing/report"), b"later source loses\n");
}

fn seed_receiver(path: &Path) -> (u64, u64) {
    directory(path);
    for subfolder in ["scope", "scope-more"] {
        directory(&path.join(subfolder));
    }
    write(&path.join("scope/obsolete"), b"previous scope\n");
    write(&path.join("scope-more/keep"), b"similarly named sibling\n");
    write(&path.join("outside"), b"unmatched top level\n");
    write(&path.join("report"), b"previous report\n");
    identity(path)
}

fn sources(path: &Path, mode: ReceiverMode) -> Vec<String> {
    let mut paths = ["first/report", "named/scope"]
        .map(|name| path.join(name).to_str().unwrap().to_owned())
        .to_vec();
    if mode != ReceiverMode::ScopedMirror {
        paths.push(format!("{}/", path.join("flat").display()));
    }
    paths.push(path.join("losing/report").to_str().unwrap().into());
    paths
}

fn target(
    fixture: &Fixture,
    upload: bool,
    destination: &Path,
    sources: Vec<String>,
    mode: ReceiverMode,
) -> MixedReviewTarget {
    let destination = if upload {
        MixedDestination::Server(
            RemotePath::try_from(destination.to_str().unwrap().to_owned()).unwrap(),
        )
    } else {
        MixedDestination::Local(destination.to_owned())
    };
    MixedReviewTarget {
        reference: fixture.reference.clone(),
        sources,
        destination,
        delete: mode == ReceiverMode::ScopedMirror,
        elevation: Elevation::Passwordless,
        review_vacant: false,
    }
}

fn preview(fixture: &Fixture, target: &MixedReviewTarget) -> Result<MixedReview, String> {
    let credentials = Credentials::resolve_directory_browser(
        &target.reference,
        fixture.connections.get(target.reference.id).unwrap(),
        &ServerAccess::default(),
        |_, _, _| Err("unexpected credential"),
    )
    .unwrap();
    rsync::preview_mixed(
        credentials,
        target,
        &fixture.trust,
        Arc::new(AtomicBool::new(false)),
        || Ok(()),
    )
}

fn handoff(review: &MixedReview) -> &str {
    match (&review.privileged, &review.receiver) {
        (Some(MixedPrivilegedReview::Upload(proof)), MixedReceiver::Server { .. }) => {
            &proof.handoff
        }
        (Some(MixedPrivilegedReview::Download(proofs)), MixedReceiver::Local { .. }) => {
            assert_eq!(proofs.len(), review.plan.sources.len());
            &proofs[0].handoff
        }
        _ => panic!("missing exact mixed elevation proof"),
    }
}

fn action(fixture: &Fixture, target: &MixedReviewTarget, review: MixedReview) -> SshCommand {
    let digest = rsync::program_digest(&fixture.program).unwrap();
    let pins = sync::fingerprints(&sync::pins(&fixture.reference, &fixture.trust).unwrap());
    let (kind, local, remote, settings) = match &target.destination {
        MixedDestination::Server(path) => (
            "rsync_upload",
            target.sources[0].as_str(),
            path.as_str(),
            "existing",
        ),
        MixedDestination::Local(path) => (
            "rsync_download",
            path.to_str().unwrap(),
            target.sources[0].as_str(),
            "existing_download",
        ),
    };
    let mut transfer = json!({"kind":kind, "directory":true, "elevation":"passwordless", "program":fixture.program,
        "local":local, "remote":remote, "mixed": target.sources,
        "approval":{"program":fixture.program, "digest":digest, "reference":fixture.reference, "pins":pins, "mixed":review}});
    transfer[settings] = json!({"include_source_name":false, "delete":target.delete});
    serde_json::from_value::<SshCommand>(json!({"connection":fixture.reference,"remote":transfer}))
        .unwrap()
        .validated()
        .unwrap()
}

fn assert_projection(destination: &Path, mode: ReceiverMode) {
    assert_eq!(fs::read(destination.join("report")).unwrap(), REPORT);
    assert_eq!(
        fs::read(destination.join("scope/new")).unwrap(),
        b"named source\n"
    );
    assert!(destination.join("scope/empty").is_dir());
    assert!(!destination.join("new").exists());
    assert_eq!(
        destination.join("flattened").is_file(),
        mode != ReceiverMode::ScopedMirror
    );
    assert_eq!(
        destination.join("scope/obsolete").is_file(),
        mode == ReceiverMode::Merge
    );
    for path in ["scope-more/keep", "outside"] {
        assert_eq!(
            destination.join(path).is_file(),
            mode != ReceiverMode::Vacant
        );
    }
}

fn assert_private_download(destination: &Path) {
    for directory in ["", "scope", "scope/empty"] {
        assert_eq!(
            fs::metadata(destination.join(directory)).unwrap().mode() & 0o777,
            0o700
        );
    }
    for file in ["report", "scope/new", "flattened"] {
        assert_eq!(
            fs::metadata(destination.join(file)).unwrap().mode() & 0o777,
            0o600
        );
    }
}

fn assert_recovered(completion: &Completion, destination: &Path, expected: (u64, u64)) {
    let path = recovered(completion, destination, expected);
    assert_eq!(fs::read(path.join("report")).unwrap(), b"previous report\n");
    assert_eq!(
        fs::read(path.join("scope/obsolete")).unwrap(),
        b"previous scope\n"
    );
}

fn recovered<'a>(completion: &'a Completion, destination: &Path, expected: (u64, u64)) -> &'a Path {
    let detail = completion.detail.as_deref().unwrap();
    let (_, rest) = detail.split_once("original is retained at ").unwrap();
    let path = Path::new(rest.split(". ").next().unwrap().trim_end_matches('.'));
    assert_eq!(path.parent().unwrap().parent(), destination.parent());
    assert_eq!(path.file_name().unwrap(), "original");
    assert_eq!(identity(path), expected);
    path
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn assert_empty_handoff(path: &str) {
    assert!(fs::read_dir(path).unwrap().next().is_none());
}
