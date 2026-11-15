#![cfg(target_os = "linux")]

use getrandom::fill;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const APP: &str = "io.github.chardonnay.korTTY";

#[test]
#[ignore = "requires the matching Flatpak runtime and SDK in a private user installation"]
fn native_flatpak_installs_desktop_and_cli_in_the_matching_runtime() {
    let scratch = Scratch::new();
    let install =
        PathBuf::from(env::var_os("FLATPAK_USER_DIR").expect("private Flatpak directory"));
    let test_root = PathBuf::from(
        env::var_os("KORTTY_FLATPAK_TEST_ROOT").expect("private test environment parent directory"),
    );
    assert!(test_root.is_absolute() && test_root.is_dir());
    let test_root = test_root.canonicalize().unwrap();
    let install = install.canonicalize().unwrap();
    assert_ne!(test_root, install);
    assert!(install.starts_with(&test_root));
    assert!(test_root.join("private-flatpak-test").is_file());
    assert!(
        !Command::new("flatpak")
            .args(["info", "--user", APP])
            .output()
            .unwrap()
            .status
            .success(),
        "expected an empty app deployment in the private installation"
    );

    let arch = env::consts::ARCH;
    let runtime = format!("org.freedesktop.Platform/{arch}/25.08");
    let runtime_path =
        stdout(Command::new("flatpak").args(["info", "--user", "--show-location", &runtime]));
    let command = Path::new(runtime_path.trim()).join("files/bin/true");
    let binaries = scratch.0.join("bin");
    fs::create_dir(&binaries).unwrap();
    for name in ["kortty", "kortty-cli"] {
        fs::copy(&command, binaries.join(name)).unwrap();
    }
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let primary = stdout(
        Command::new(env!("CARGO_BIN_EXE_kortty-package"))
            .current_dir(project)
            .args([
                "--target",
                &format!("{arch}-unknown-linux-gnu"),
                "--bin-dir",
            ])
            .arg(&binaries)
            .arg("--output-dir")
            .arg(scratch.0.join("out"))
            .arg("--linux-flatpak"),
    );
    let release = Path::new(primary.trim()).parent().unwrap();
    let bundles = fs::read_dir(release)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "flatpak")
        })
        .collect::<Vec<_>>();
    let [bundle] = bundles.as_slice() else {
        panic!("expected one Flatpak bundle, found {bundles:?}");
    };
    let name = bundle.file_name().unwrap().to_str().unwrap();
    let sha = format!("{:x}  {name}\n", Sha256::digest(fs::read(bundle).unwrap()));
    assert_eq!(
        fs::read_to_string(release.join(format!("{name}.sha256"))).unwrap(),
        sha
    );

    let _deployment = Deployment;
    success(
        Command::new("flatpak")
            .args([
                "install",
                "--user",
                "--noninteractive",
                "--no-deps",
                "--no-related",
                "--bundle",
            ])
            .arg(bundle),
    );
    let meta = stdout(Command::new("flatpak").args(["info", "--user", "--show-metadata", APP]));
    for expected in [
        format!("runtime={runtime}"),
        "command=kortty".to_owned(),
        "org.freedesktop.Notifications=talk".to_owned(),
    ] {
        assert!(meta.lines().any(|line| line == expected), "{meta}");
    }
    assert!(!meta.contains("org.freedesktop.Flatpak=talk"), "{meta}");
    let installed =
        stdout(Command::new("flatpak").args(["info", "--user", "--show-location", APP]));
    let installed = Path::new(installed.trim());
    for path in [
        "files/bin/kortty".to_owned(),
        "files/bin/kortty-cli".to_owned(),
        format!("export/share/applications/{APP}.desktop"),
        format!("export/share/icons/hicolor/64x64/apps/{APP}.png"),
        format!("files/share/metainfo/{APP}.metainfo.xml"),
        format!("files/share/licenses/{APP}/LICENSE"),
    ] {
        assert!(installed.join(&path).is_file(), "missing {path}");
    }
    let desktop =
        fs::read_to_string(installed.join(format!("export/share/applications/{APP}.desktop")))
            .unwrap();
    assert!(
        desktop
            .lines()
            .any(|line| line == format!("StartupWMClass={APP}")),
        "{desktop}"
    );
    let home = scratch.0.join("home");
    fs::create_dir(&home).unwrap();
    success(Command::new("flatpak").env("HOME", home).args([
        "run",
        "--user",
        "--command=kortty-cli",
        APP,
    ]));
}

fn success(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(command: &mut Command) -> String {
    String::from_utf8(success(command).stdout).unwrap()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut random = [0; 16];
        fill(&mut random).unwrap();
        let path = env::temp_dir().join(format!(
            "kortty-flatpak-{:032x}",
            u128::from_le_bytes(random)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Deployment;

impl Drop for Deployment {
    fn drop(&mut self) {
        let _ = Command::new("flatpak")
            .args([
                "uninstall",
                "--user",
                "--noninteractive",
                "--no-related",
                APP,
            ])
            .output();
    }
}
