//! Packages an already signed macOS app without modifying its bundle.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::{
    Platform, Result, Staging, Target, checksum, command, copy_licenses, ensure_new, invalid,
    verify_macos_signature,
};

pub(super) fn package(
    root: &Path,
    output: &Path,
    target: &Target,
    triple: &str,
    version: &str,
    app: &Path,
) -> Result<PathBuf> {
    require_macos(target)?;
    let app = std::path::absolute(app)?;
    verify_bundle(&app, target, version)?;
    let source_ticket = ticket_hash(&app)?;

    let output = std::path::absolute(output)?;
    let stem = format!("kortty-{version}-{triple}");
    let final_dir = output.join(format!("{stem}-dmg"));
    ensure_new(&final_dir)?;
    fs::create_dir_all(&output)?;
    let staging = Staging::new(&output)?;
    let contents = staging.0.join("contents");
    fs::create_dir(&contents)?;
    let copied = contents.join("kortty.app");
    command(
        Command::new("/usr/bin/ditto")
            .args(["--rsrc", "--extattr", "--acl"])
            .arg(app)
            .arg(&copied),
        "copy the signed macOS app bundle",
    )?;
    verify_bundle(&copied, target, version)?;
    if ticket_hash(&copied)? != source_ticket {
        return Err(invalid("The copied app bundle's notarization ticket changed").into());
    }
    copy_licenses(root, &contents)?;
    applications_link(&contents.join("Applications"))?;

    let release = staging.0.join("release");
    fs::create_dir(&release)?;
    let name = format!("{stem}.dmg");
    let image = release.join(&name);
    command(
        Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-srcfolder"])
            .arg(contents)
            .args(["-fs", "HFS+", "-volname", "kortty", "-format", "UDZO"])
            .arg(&image),
        "create the read-only macOS disk image",
    )?;
    command(
        Command::new("/usr/bin/hdiutil")
            .args(["verify", "-quiet"])
            .arg(image),
        "verify the macOS disk image checksum",
    )?;
    checksum(&release, &name)?;
    fs::rename(release, &final_dir)?;
    Ok(final_dir.join(name))
}

fn require_macos(target: &Target) -> Result<()> {
    if target.platform != Platform::Macos || !cfg!(target_os = "macos") {
        return Err(invalid("--macos-dmg requires a macOS host and target").into());
    }
    Ok(())
}

fn verify_bundle(app: &Path, target: &Target, version: &str) -> Result<()> {
    if app.file_name() != Some(OsStr::new("kortty.app")) {
        return Err(invalid("--macos-dmg requires a prepared kortty.app bundle").into());
    }
    let contents = entry(&entry(app, true)?.join("Contents"), true)?;
    let executables = entry(&contents.join("MacOS"), true)?;
    let helpers = entry(&contents.join("Helpers"), true)?;
    let worker = entry(&helpers.join("kortty-ssh-worker.app"), true)?;
    let worker_contents = entry(&worker.join("Contents"), true)?;
    let worker_executables = entry(&worker_contents.join("MacOS"), true)?;
    let cli = executables.join("kortty-cli");
    for binary in [
        executables.join("kortty"),
        cli.clone(),
        worker_executables.join("kortty-ssh-worker"),
    ] {
        target.verify_binary(&entry(&binary, false)?)?;
    }
    verify_plist(&contents, "CFBundleExecutable", "kortty")?;
    verify_plist(&contents, "CFBundleIdentifier", "de.kortty.rust")?;
    verify_plist(&contents, "CFBundleShortVersionString", version)?;
    verify_plist(&worker_contents, "CFBundleExecutable", "kortty-ssh-worker")?;
    verify_plist(
        &worker_contents,
        "CFBundleIdentifier",
        "de.kortty.rust.ssh-worker",
    )?;
    verify_macos_signature(&cli)?;
    verify_macos_signature(&worker)?;
    verify_macos_signature(app)
}

fn entry(path: &Path, directory: bool) -> Result<PathBuf> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if (directory && kind.is_dir()) || (!directory && kind.is_file()) {
        Ok(path.to_path_buf())
    } else {
        Err(invalid(format!("Unexpected app bundle entry at {}", path.display())).into())
    }
}

fn verify_plist(contents: &Path, name: &str, expected: &str) -> Result<()> {
    let path = entry(&contents.join("Info.plist"), false)?;
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", name, "raw", "-expect", "string", "-o", "-"])
        .arg(&path)
        .output()?;
    let expected = expected.as_bytes();
    let value = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    if !output.status.success() || value != expected {
        return Err(invalid(format!("Unexpected {name} in {}", path.display())).into());
    }
    Ok(())
}

fn ticket_hash(app: &Path) -> Result<Option<[u8; 32]>> {
    let path = app.join("Contents/CodeResources");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            let mut hash = Sha256::new();
            io::copy(&mut File::open(path)?, &mut hash)?;
            Ok(Some(hash.finalize().into()))
        }
        Ok(_) => Err(invalid("The app notarization ticket is not a regular file").into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn applications_link(path: &Path) -> Result<()> {
    std::os::unix::fs::symlink("/Applications", path)?;
    Ok(())
}

#[cfg(not(unix))]
fn applications_link(_: &Path) -> Result<()> {
    Err(invalid("macOS disk images require a macOS host").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmg_preflight_rejects_non_macos_hosts_or_targets() {
        for triple in ["x86_64-unknown-linux-gnu", "aarch64-pc-windows-msvc"] {
            assert!(require_macos(&Target::parse(triple).unwrap()).is_err());
        }
        let target = Target::parse("aarch64-apple-darwin").unwrap();
        assert_eq!(require_macos(&target).is_ok(), cfg!(target_os = "macos"));
    }
}
