use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const X64: u16 = 0x8664;
const ARM64: u16 = 0xaa64;
const EXECUTABLE: u16 = 0x0002;
const DLL: u16 = 0x2000;

#[test]
fn windows_release_requires_the_matching_worker_and_archives_it_with_the_app() {
    let fixture = Fixture::new();
    let binaries = fixture.0.join("binaries");
    let output = fixture.0.join("output");
    fs::create_dir(&binaries).unwrap();
    let programs = [
        ("kortty.exe", executable(ARM64, EXECUTABLE, b"desktop")),
        ("kortty-cli.exe", executable(ARM64, EXECUTABLE, b"control")),
        (
            "kortty-ssh-worker.exe",
            executable(ARM64, EXECUTABLE, b"confined worker"),
        ),
    ];
    for (name, contents) in &programs[..2] {
        fs::write(binaries.join(name), contents).unwrap();
    }
    let (worker_name, worker) = &programs[2];
    let worker_path = binaries.join(worker_name);

    let check_rejected = || {
        let result = package(&binaries, &output);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(worker_name));
        assert!(!output.exists(), "validation must precede publication");
    };
    check_rejected();
    fs::write(&worker_path, executable(X64, EXECUTABLE, b"wrong machine")).unwrap();
    check_rejected();
    fs::write(&worker_path, executable(ARM64, EXECUTABLE | DLL, b"DLL")).unwrap();
    check_rejected();
    fs::write(&worker_path, worker).unwrap();

    let result = package(&binaries, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let path = PathBuf::from(String::from_utf8(result.stdout).unwrap().trim());
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("zip")
    );
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
    for (name, expected) in &programs {
        let mut file = archive.by_name(&format!("{stem}/{name}")).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes, expected);
        assert_eq!(file.unix_mode().unwrap() & 0o777, 0o755);
    }
    let executables = archive
        .file_names()
        .map(|name| name.unwrap())
        .filter(|name| name.ends_with(".exe"))
        .count();
    assert_eq!(executables, programs.len());
    assert!(path.with_extension("zip.sha256").is_file());
}

fn package(binaries: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kortty-package"))
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .args(["--target", "aarch64-pc-windows-msvc", "--bin-dir"])
        .arg(binaries)
        .arg("--output-dir")
        .arg(output)
        .output()
        .unwrap()
}

fn executable(machine: u16, characteristics: u16, payload: &[u8]) -> Vec<u8> {
    // Test doubles for native artifacts. Only the DOS, COFF and PE32+ header
    // fields used by the packager are needed. These files are never executed.
    let mut bytes = vec![0; 90];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[68..70].copy_from_slice(&machine.to_le_bytes());
    bytes[86..88].copy_from_slice(&characteristics.to_le_bytes());
    bytes[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "kortty-windows-package-{:032x}",
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
