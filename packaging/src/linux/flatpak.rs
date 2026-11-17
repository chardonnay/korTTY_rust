//! Flatpak release bundles for prebuilt native Linux executables.

use std::fs;
use std::path::Path;

use super::{copy_tree, normalize, output, tool};
use crate::{Result, invalid, target};

const APP_ID: &str = "io.github.chardonnay.korTTY";
const APP_BRANCH: &str = "stable";
const RUNTIME: &str = "org.freedesktop.Platform";
const SDK: &str = "org.freedesktop.Sdk";
const RUNTIME_BRANCH: &str = "25.08";
const RUNTIME_REPO: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
const DESKTOP: &[u8] = include_bytes!("../../linux/flatpak/kortty.desktop");
const METAINFO: &[u8] = include_bytes!("../../linux/flatpak/kortty.metainfo.xml");
const ICONS: [(&str, &[u8]); 2] = [
    (
        "64x64",
        include_bytes!("../../linux/flatpak/kortty_icon_64.png"),
    ),
    (
        "128x128",
        include_bytes!("../../linux/flatpak/kortty_icon_128.png"),
    ),
];

// Match the Java Flatpak's host file, serial port and host shell access.
const PERMISSIONS: &[&str] = &[
    "--no-inherit-permissions",
    "--share=network",
    "--share=ipc",
    "--socket=wayland",
    "--socket=fallback-x11",
    "--device=all",
    "--filesystem=host",
    "--talk-name=org.freedesktop.Notifications",
    "--talk-name=org.freedesktop.Flatpak",
    "--env=SHELL=/bin/bash",
];

pub(super) fn require_tools(arch: &str) -> Result<()> {
    let actual = output(tool("flatpak").arg("--default-arch"))?;
    if actual.trim() != arch {
        return Err(invalid("The Flatpak package architecture differs from the build host").into());
    }
    for id in [RUNTIME, SDK] {
        let wanted = format!("runtime/{id}/{arch}/{RUNTIME_BRANCH}");
        let installed = output(tool("flatpak").args(["info", "--show-ref", &wanted]));
        if !installed.is_ok_and(|reference| reference.trim() == wanted) {
            return Err(invalid(format!("Install Flatpak {wanted} before packaging")).into());
        }
    }
    Ok(())
}

pub(super) fn build(
    staging: &Path,
    release: &Path,
    payload: &Path,
    version: &str,
    arch: &str,
) -> Result<String> {
    let work = fs::canonicalize(staging)?.join("flatpak-build");
    let build = work.join("app");
    let repository = work.join("repository");
    let architecture = format!("--arch={arch}");
    fs::create_dir(&work)?;
    output(
        tool("flatpak")
            .args(["build-init", &architecture])
            .arg(&build)
            .args([APP_ID, SDK, RUNTIME, RUNTIME_BRANCH]),
    )?;
    stage(payload, &build.join("files"))?;
    check_runtime(&build)?;
    output(
        tool("flatpak")
            .args(["build-finish", "--command=kortty"])
            .args(PERMISSIONS)
            .arg(&build),
    )?;
    output(
        tool("flatpak")
            .args([
                "build-export",
                &architecture,
                "--timestamp=2026-01-01T00:00:00Z",
            ])
            .arg(&repository)
            .arg(&build)
            .arg(APP_BRANCH),
    )?;
    let name = format!("kortty-{version}-{arch}.flatpak");
    output(
        tool("flatpak")
            .args([
                "build-bundle",
                &architecture,
                &format!("--runtime-repo={RUNTIME_REPO}"),
            ])
            .arg(repository)
            .arg(release.join(&name))
            .args([APP_ID, APP_BRANCH]),
    )?;
    Ok(name)
}

fn stage(payload: &Path, files: &Path) -> Result<()> {
    let license = files.join("share/licenses").join(APP_ID);
    copy_tree(&payload.join("bin"), &files.join("bin"))?;
    copy_tree(&payload.join("licenses"), &license.join("third-party"))?;
    fs::copy(payload.join("license.txt"), license.join("LICENSE"))?;
    let applications = files.join("share/applications");
    let metainfo = files.join("share/metainfo");
    fs::create_dir_all(&applications)?;
    fs::create_dir_all(&metainfo)?;
    fs::write(applications.join(format!("{APP_ID}.desktop")), DESKTOP)?;
    fs::write(metainfo.join(format!("{APP_ID}.metainfo.xml")), METAINFO)?;
    for (size, icon) in ICONS {
        let directory = files.join("share/icons/hicolor").join(size).join("apps");
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(format!("{APP_ID}.png")), icon)?;
    }
    normalize(files)?;
    Ok(())
}

fn check_runtime(build: &Path) -> Result<()> {
    output(
        tool("flatpak")
            .args(["build", "--runtime", "--readonly", "--die-with-parent"])
            .arg(build)
            .args([
                "/bin/sh",
                "-ec",
                r#"
test -x /bin/bash
test -x /usr/bin/flatpak-spawn
for program do
    if ! linked=$(LC_ALL=C /usr/bin/ldd -r "/app/bin/$program" 2>&1); then
        printf '%s\n' "$linked" >&2
        exit 1
    fi
    case "$linked" in
        *'not found'*|*'undefined symbol'*)
            printf '%s\n' "$linked" >&2
            exit 1
            ;;
    esac
done
"#,
                "flatpak-link-check",
            ])
            .args(target::UNIX_PROGRAMS),
    )
    .map(|_| ())
    .map_err(|error| {
        invalid(format!(
            "Executables must load in Flatpak {RUNTIME}//{RUNTIME_BRANCH}. Build against the matching SDK: {error}"
        ))
        .into()
    })
}
