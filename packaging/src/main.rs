//! Stages native kortty binaries and creates deterministic release archives.

use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};
use toml_edit::DocumentMut;

mod archive;
mod linux;
#[path = "../../src/native_executable.rs"]
mod native_executable;
mod target;

use target::{Platform, Target};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "usage: kortty-package [--target <rust-target>] [--bin-dir <directory>]
                      [--output-dir <directory> | --stage-app <kortty.app>]
                      [--linux-zip] [--linux-deb] [--linux-rpm]

Run from the kortty workspace root.
Defaults to the native Rust target, target/release, and target/dist.
For Linux releases, --linux-zip also creates a portable ZIP and its checksum.
Use --linux-deb on a matching Debian build host with dpkg-dev and dpkg-deb.
Use --linux-rpm on a matching Linux build host with rpmbuild and ELF dependency generators.
The macOS --stage-app mode creates a signed local app bundle instead of an archive.";

struct Options {
    target: Option<String>,
    bin_dir: PathBuf,
    destination: Destination,
    linux: linux::Packages,
}

enum Destination {
    Archive(PathBuf),
    App(PathBuf),
}

impl Options {
    fn parse(root: &Path, mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>> {
        let mut target = None;
        let mut bin_dir = None;
        let mut destination = None;
        let mut linux = linux::Packages::default();
        while let Some(flag) = args.next() {
            if flag == "--help" || flag == "-h" {
                return Ok(None);
            }
            let value = || invalid(format!("missing value for {}", flag.to_string_lossy()));
            match flag.to_str() {
                Some("--target") if target.is_none() => {
                    target = Some(
                        args.next()
                            .ok_or_else(value)?
                            .into_string()
                            .map_err(|_| invalid("Rust target must be UTF-8"))?,
                    );
                }
                Some("--bin-dir") if bin_dir.is_none() => {
                    bin_dir = Some(PathBuf::from(args.next().ok_or_else(value)?));
                }
                Some("--output-dir") if destination.is_none() => {
                    destination = Some(Destination::Archive(PathBuf::from(
                        args.next().ok_or_else(value)?,
                    )));
                }
                Some("--stage-app") if destination.is_none() => {
                    destination = Some(Destination::App(PathBuf::from(
                        args.next().ok_or_else(value)?,
                    )));
                }
                Some("--linux-zip") if !linux.zip => linux.zip = true,
                Some("--linux-deb") if !linux.deb => linux.deb = true,
                Some("--linux-rpm") if !linux.rpm => linux.rpm = true,
                _ => {
                    return Err(invalid(format!(
                        "unknown, repeated, or conflicting option {}",
                        flag.to_string_lossy()
                    ))
                    .into());
                }
            }
        }
        Ok(Some(Self {
            target,
            bin_dir: bin_dir.unwrap_or_else(|| root.join("target/release")),
            destination: destination
                .unwrap_or_else(|| Destination::Archive(root.join("target/dist"))),
            linux,
        }))
    }
}

/// Owns a private, disposable directory on the same volume as the output.
struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> Result<Self> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let path = parent.join(format!(
            ".kortty-package-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kortty-package: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let root = env::current_dir()?;
    let Some(options) = Options::parse(&root, env::args_os().skip(1))? else {
        println!("{USAGE}");
        return Ok(());
    };
    let triple = match options.target {
        Some(triple) => triple,
        None => target::native()?,
    };
    let target = Target::parse(&triple)?;
    if options.linux.selected()
        && (target.platform != Platform::Linux
            || !matches!(&options.destination, Destination::Archive(_)))
    {
        return Err(invalid(format!(
            "{} requires Linux archive output",
            options.linux.option()
        ))
        .into());
    }
    options.linux.require_host(&triple)?;
    let version = version(&root)?;
    let binaries = target.binaries(&options.bin_dir)?;
    match options.destination {
        Destination::Archive(directory) => {
            let release = package(
                &root,
                &directory,
                &target,
                &triple,
                &version,
                &binaries,
                options.linux,
            )?;
            println!("{}", release.display());
        }
        Destination::App(path) => {
            if target.platform != Platform::Macos
                || path.file_name() != Some(OsStr::new("kortty.app"))
            {
                return Err(
                    invalid("--stage-app requires a macOS target and a kortty.app path").into(),
                );
            }
            let path = std::path::absolute(path)?;
            ensure_new(&path)?;
            let parent = path
                .parent()
                .ok_or_else(|| invalid("app parent directory missing"))?;
            fs::create_dir_all(parent)?;
            let staging = Staging::new(parent)?;
            let app = staging.0.join("kortty.app");
            macos_bundle(&root, &app, &version, &binaries)?;
            fs::rename(app, &path)?;
            println!("{}", path.display());
        }
    }
    Ok(())
}

fn version(root: &Path) -> Result<String> {
    let path = root.join("Cargo.toml");
    let manifest = fs::read_to_string(&path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?
        .parse::<DocumentMut>()?;
    let package = manifest
        .get("package")
        .and_then(|item| item.as_table_like())
        .ok_or_else(|| invalid("run from the kortty workspace root"))?;
    if package.get("name").and_then(|name| name.as_str()) != Some("kortty") {
        return Err(invalid("run from the kortty workspace root").into());
    }
    let version = package
        .get("version")
        .and_then(|version| version.as_str())
        .ok_or_else(|| invalid("kortty package version missing"))?;
    if version.is_empty()
        || !version.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-+".contains(&byte)
        })
    {
        return Err(invalid("kortty package version is not safe for an archive name").into());
    }
    Ok(version.to_owned())
}

fn package(
    root: &Path,
    output: &Path,
    target: &Target,
    triple: &str,
    version: &str,
    binaries: &[PathBuf],
    linux: linux::Packages,
) -> Result<PathBuf> {
    let stem = format!("kortty-{version}-{triple}");
    let final_dir = output.join(&stem);
    ensure_new(&final_dir)?;
    fs::create_dir_all(output)?;
    let staging = Staging::new(output)?;
    let payload = staging.0.join("payload").join(&stem);
    fs::create_dir_all(&payload)?;
    match target.platform {
        Platform::Linux => {
            copy_binaries(binaries, &payload.join("bin"))?;
            let applications = payload.join("share/applications");
            let icons = payload.join("share/pixmaps");
            fs::create_dir_all(&applications)?;
            fs::create_dir_all(&icons)?;
            fs::copy(
                root.join("packaging/linux/kortty.desktop"),
                applications.join("kortty.desktop"),
            )?;
            fs::copy(
                root.join("src/workspace/coding_agent_app_icon/kortty_icon.png"),
                icons.join("kortty.png"),
            )?;
        }
        Platform::Windows => copy_binaries(binaries, &payload)?,
        Platform::Macos => macos_bundle(root, &payload.join("kortty.app"), version, binaries)?,
    }
    copy_licenses(root, &payload)?;

    let release = staging.0.join("release");
    fs::create_dir(&release)?;
    let format = if target.platform == Platform::Linux {
        archive::Format::TarGz
    } else {
        archive::Format::Zip
    };
    let name = write_archive(format, &stem, &release, &payload)?;
    if linux.zip {
        write_archive(archive::Format::Zip, &stem, &release, &payload)?;
    }
    for native in linux.write(triple, version, &payload, &staging.0, &release)? {
        checksum(&release, &native)?;
    }
    fs::rename(release, &final_dir)?;
    Ok(final_dir.join(name))
}

fn write_archive(
    format: archive::Format,
    stem: &str,
    release: &Path,
    payload: &Path,
) -> Result<String> {
    let name = format!("{stem}.{}", format.extension());
    let archive = release.join(&name);
    archive::write(format, &archive, payload)?;
    checksum(release, &name)?;
    Ok(name)
}

fn checksum(release: &Path, name: &str) -> Result<()> {
    let mut hash = Sha256::new();
    io::copy(&mut File::open(release.join(name))?, &mut hash)?;
    fs::write(
        release.join(format!("{name}.sha256")),
        format!("{:x}  {name}\n", hash.finalize()),
    )?;
    Ok(())
}

fn copy_binaries(binaries: &[PathBuf], destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for source in binaries {
        let name = source
            .file_name()
            .ok_or_else(|| invalid("binary filename missing"))?;
        let path = destination.join(name);
        fs::copy(source, &path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

fn copy_licenses(root: &Path, payload: &Path) -> Result<()> {
    fs::copy(root.join("LICENSE"), payload.join("license.txt"))?;
    copy_vendor_licenses(
        &root.join("src/workspace/ascii_art/fonts"),
        &payload.join("licenses/figlet-fonts"),
    )?;
    copy_vendor_licenses(
        &root.join("src/session_journal/export/pdf/fonts"),
        &payload.join("licenses/noto-fonts"),
    )?;
    copy_vendor_licenses(&root.join("third-party/7z"), &payload.join("licenses/7z"))?;
    let latex_source = root.join("third-party/latex-rust");
    let latex_notices = payload.join("licenses/latex-rust");
    copy_vendor_licenses(&latex_source, &latex_notices)?;
    fs::copy(
        latex_source.join("dvipsnames.tsv"),
        latex_notices.join("dvipsnames.tsv"),
    )?;
    for crate_dir in fs::read_dir(root.join("vendor"))? {
        let crate_dir = crate_dir?;
        let source = crate_dir.path();
        if crate_dir.file_type()?.is_dir() && source.join("Cargo.toml").is_file() {
            copy_vendor_licenses(
                &source,
                &payload.join("licenses").join(crate_dir.file_name()),
            )?;
        }
    }
    Ok(())
}

fn copy_vendor_licenses(source: &Path, destination: &Path) -> Result<()> {
    let mut found = false;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let category = name.split(['-', '_', '.']).next();
        if !matches!(
            category,
            Some("license" | "licence" | "copying" | "notice" | "copyright")
        ) || !entry.file_type()?.is_file()
        {
            continue;
        }
        fs::create_dir_all(destination)?;
        let path = if Path::new(&name).extension().is_some() {
            destination.join(name)
        } else {
            destination.join(format!("{name}.txt"))
        };
        ensure_new(&path)?;
        fs::copy(entry.path(), path)?;
        found = true;
    }
    if !found {
        return Err(invalid(format!("no license notice in {}", source.display())).into());
    }
    Ok(())
}

fn macos_bundle(root: &Path, app: &Path, version: &str, binaries: &[PathBuf]) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(invalid("macOS bundles must be staged on macOS for codesigning").into());
    }
    let contents = app.join("Contents");
    let executable_dir = contents.join("MacOS");
    let (worker, desktop_binaries) = binaries
        .split_last()
        .filter(|(worker, _)| worker.file_name() == Some(OsStr::new("kortty-ssh-worker")))
        .ok_or_else(|| invalid("macOS SSH worker missing"))?;
    copy_binaries(desktop_binaries, &executable_dir)?;
    let helper = contents.join("Helpers/kortty-ssh-worker.app");
    let helper_contents = helper.join("Contents");
    copy_binaries(std::slice::from_ref(worker), &helper_contents.join("MacOS"))?;
    fs::copy(
        root.join("packaging/macos/WorkerInfo.plist"),
        helper_contents.join("Info.plist"),
    )?;
    let signing = Staging::new(
        app.parent()
            .ok_or_else(|| invalid("app parent is missing"))?,
    )?;
    let entitlements = signing.0.join("Worker.entitlements");
    // AMFI's XML parser rejects CRLF even when plutil accepts the property list.
    command(
        Command::new("/usr/bin/plutil")
            .args(["-convert", "xml1", "-o"])
            .arg(&entitlements)
            .arg(root.join("packaging/macos/Worker.entitlements")),
        "normalize macOS SSH sandbox entitlements",
    )?;
    command(
        Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--sign",
                "-",
                "--timestamp=none",
                "--options",
                "runtime",
            ])
            .arg("--entitlements")
            .arg(entitlements)
            .arg(&helper),
        "sign macOS SSH sandbox worker",
    )?;
    let plist = contents.join("Info.plist");
    fs::copy(root.join("packaging/macos/Info.plist"), &plist)?;
    command(
        Command::new("/usr/bin/plutil")
            .args(["-replace", "CFBundleShortVersionString", "-string", version])
            .arg(plist),
        "write bundle version",
    )?;
    for path in [&executable_dir.join("kortty-cli"), app] {
        command(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--timestamp=none"])
                .arg(path),
            "sign macOS bundle",
        )?;
    }
    command(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--deep"])
            .arg(app),
        "verify macOS bundle",
    )
}

fn command(command: &mut Command, action: &str) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other(format!("{action} failed ({status})")).into());
    }
    Ok(())
}

fn ensure_new(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists at {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
