use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const TARGET: &str = "aarch64-unknown-linux-gnu";

#[test]
fn linux_release_option_adds_repeatable_portable_zip_with_unix_modes() {
    let fixture = Fixture::new();
    let binaries = fixture.0.join("binaries");
    fs::create_dir(&binaries).unwrap();
    let programs = [
        ("kortty", executable(b"desktop")),
        ("kortty-cli", executable(b"control")),
    ];
    for (name, contents) in &programs {
        fs::write(binaries.join(name), contents).unwrap();
    }

    let output = fixture.0.join("default");
    let default_tar = release_path(package(TARGET, &binaries, &output, false));
    let name = default_tar.file_name().unwrap().to_str().unwrap();
    let stem = name.strip_suffix(".tar.gz").unwrap();
    let zip_name = format!("{stem}.zip");
    assert!(!default_tar.with_file_name(&zip_name).exists());
    assert_checksum(&default_tar);

    let portable_tar = release_path(package(
        TARGET,
        &binaries,
        &fixture.0.join("portable"),
        true,
    ));
    assert_eq!(portable_tar.file_name(), default_tar.file_name());
    assert_eq!(
        fs::read(&portable_tar).unwrap(),
        fs::read(&default_tar).unwrap()
    );
    assert_checksum(&portable_tar);
    let portable_zip = portable_tar.with_file_name(&zip_name);
    assert_checksum(&portable_zip);
    let repeated_tar = release_path(package(TARGET, &binaries, &fixture.0.join("repeat"), true));
    assert_eq!(
        fs::read(&portable_zip).unwrap(),
        fs::read(repeated_tar.with_file_name(&zip_name)).unwrap()
    );

    let mut zip = zip::ZipArchive::new(File::open(&portable_zip).unwrap()).unwrap();
    for (name, expected) in &programs {
        let mut entry = zip.by_name(&format!("{stem}/bin/{name}")).unwrap();
        assert_eq!(entry.unix_mode().unwrap() & 0o777, 0o755);
        assert_eq!(entry.last_modified(), Some(zip::DateTime::default()));
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes, expected);
    }
    for relative in [
        "license.txt",
        "share/applications/kortty.desktop",
        "share/pixmaps/kortty.png",
    ] {
        let entry = zip.by_name(&format!("{stem}/{relative}")).unwrap();
        assert_eq!(entry.unix_mode().unwrap() & 0o777, 0o644);
    }
}

#[test]
fn linux_zip_rejects_non_linux_targets_before_publication() {
    let fixture = Fixture::new();
    let output = fixture.0.join("output");
    let result = package("aarch64-pc-windows-msvc", &fixture.0, &output, true);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("--linux-zip requires Linux archive output")
    );
    assert!(!output.exists());
}

#[test]
fn native_linux_flags_reject_non_linux_targets_before_publication() {
    let fixture = Fixture::new();
    let output = fixture.0.join("output");
    for flag in ["--linux-deb", "--linux-rpm"] {
        let result = package_command("aarch64-pc-windows-msvc", &fixture.0, &output)
            .arg(flag)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains(&format!("{flag} requires Linux archive output"))
        );
        assert!(!output.exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires native Debian packaging tools and shared-library metadata"]
fn native_deb_contains_runnable_binaries_and_versioned_dependencies() {
    let fixture = Fixture::new();
    let package = native_package(&fixture, "--linux-deb", "deb");
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let field = |name: &str| {
        stdout(
            Command::new("dpkg-deb")
                .arg("--field")
                .arg(&package)
                .arg(name),
        )
    };
    assert_eq!(field("Architecture").trim(), arch);
    assert!(field("Depends").contains("libc6 (>= "));
    assert!(field("Depends").contains("libxkbcommon-x11-0"));
    let files = stdout(Command::new("dpkg-deb").arg("--contents").arg(&package));
    assert_payload(&files, "/usr/share/doc/kortty/copyright");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires native RPM packaging tools and ELF dependency generators"]
fn native_rpm_contains_runnable_binaries_and_versioned_dependencies() {
    let fixture = Fixture::new();
    let package = native_package(&fixture, "--linux-rpm", "rpm");
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let format = |format: &str| {
        stdout(
            Command::new("rpm")
                .args(["-qp", "--queryformat", format])
                .arg(&package),
        )
    };
    assert_eq!(format("%{ARCH}"), arch);
    let dependencies = stdout(
        Command::new("rpm")
            .args(["-qp", "--requires"])
            .arg(&package),
    );
    assert!(dependencies.contains("libc.so.6(GLIBC_"));
    assert!(dependencies.contains("libxkbcommon-x11.so.0()(64bit)"));
    let files = format("[%{FILEMODES:perms} %{FILENAMES}\n]");
    assert_payload(&files, "/usr/share/licenses/kortty/LICENSE");
}

#[cfg(target_os = "linux")]
fn native_package(fixture: &Fixture, flag: &str, extension: &str) -> PathBuf {
    let binaries = fixture.0.join("binaries");
    fs::create_dir(&binaries).unwrap();
    for name in ["kortty", "kortty-cli"] {
        fs::copy("/bin/true", binaries.join(name)).unwrap();
    }
    let target = if cfg!(target_arch = "aarch64") {
        TARGET
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let create = |directory: &str| {
        let tar = release_path(
            package_command(target, &binaries, &fixture.0.join(directory))
                .arg(flag)
                .output()
                .unwrap(),
        );
        assert_checksum(&tar);
        fs::read_dir(tar.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|value| value == extension))
            .unwrap()
    };
    let package = create("first");
    assert_checksum(&package);
    let repeated = create("second");
    assert_eq!(fs::read(&package).unwrap(), fs::read(repeated).unwrap());
    package
}

#[cfg(target_os = "linux")]
fn assert_payload(files: &str, license: &str) {
    for path in ["/usr/bin/kortty", "/usr/bin/kortty-cli"] {
        assert!(
            files
                .lines()
                .any(|line| line.starts_with("-rwxr-xr-x") && line.ends_with(path)),
            "{files}"
        );
    }
    for path in [
        "/usr/share/applications/kortty.desktop",
        "/usr/share/pixmaps/kortty.png",
        license,
    ] {
        assert!(
            files
                .lines()
                .any(|line| line.starts_with("-rw-r--r--") && line.ends_with(path)),
            "{files}"
        );
    }
}

#[cfg(target_os = "linux")]
fn stdout(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn package(target: &str, binaries: &Path, output: &Path, linux_zip: bool) -> Output {
    let mut command = package_command(target, binaries, output);
    if linux_zip {
        command.arg("--linux-zip");
    }
    command.output().unwrap()
}

fn package_command(target: &str, binaries: &Path, output: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kortty-package"));
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .args(["--target", target, "--bin-dir"])
        .arg(binaries)
        .arg("--output-dir")
        .arg(output);
    command
}

fn release_path(result: Output) -> PathBuf {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    PathBuf::from(String::from_utf8(result.stdout).unwrap().trim())
}

fn assert_checksum(path: &Path) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let expected = format!("{:x}  {name}\n", Sha256::digest(fs::read(path).unwrap()));
    assert_eq!(
        fs::read_to_string(path.with_file_name(format!("{name}.sha256"))).unwrap(),
        expected
    );
}

fn executable(payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "kortty-linux-package-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
