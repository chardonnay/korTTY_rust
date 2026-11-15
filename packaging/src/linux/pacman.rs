//! Packages prebuilt Linux executables with the native Arch tools.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use super::{MAINTAINER, output, stage_install, tool};
use crate::{Result, invalid, target};

const RUNTIME: &str = "'ca-certificates' 'libgcc' 'fontconfig' 'ttf-dejavu' 'wayland' 'libx11' 'libxcb' 'libxcursor' 'libxi' 'libxkbcommon' 'libxkbcommon-x11' 'libxrandr' 'xkeyboard-config' 'glib2' 'dbus'";

pub(super) fn require_tools(arch: &str) -> Result<()> {
    let configured = output(tool("pacman-conf").arg("Architecture"))?;
    if std::env::consts::ARCH != arch || !configured.split_whitespace().any(|value| value == arch) {
        return Err(invalid("The Pacman package architecture differs from the build host").into());
    }
    for name in ["makepkg", "pacman", "readelf"] {
        output(tool(name).arg("--version"))?;
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
    let version = release_version(version)?;
    let (glibc, openssl) = elf_requirements(payload)?;
    let work = staging.join("pacman-build");
    let source = work.join("files");
    stage_install(payload, &source, true)?;
    let work = fs::canonicalize(work)?;
    fs::write(
        work.join("PKGBUILD"),
        format!(
            r#"pkgname=kortty
pkgver={version}
pkgrel=1
pkgdesc='Native terminal emulator and SSH client'
arch=('{arch}')
url='https://github.com/chardonnay/korTTY_rust'
license=('MIT')
depends=('glibc>={glibc}' {RUNTIME} {openssl})
options=('!strip' '!debug' '!purge')

package() {{
    cp -a -- "$KORTTY_PACMAN_FILES/." "$pkgdir/"
}}
"#
        ),
    )?;
    let config = work.join("makepkg.conf");
    fs::write(
        &config,
        // Some makepkg versions accept autodeps only in the configuration.
        // Preserve other native settings and keep the explicit dependencies above.
        ". /etc/makepkg.conf\nOPTIONS=(\"${OPTIONS[@]/#autodeps/!autodeps}\")\nCOMPRESSZST=(zstd -q -c -T1)\n",
    )?;
    output(
        tool("makepkg")
            .current_dir(&work)
            .env("CARCH", arch)
            .env("PACKAGER", MAINTAINER)
            .env("PKGEXT", ".pkg.tar.zst")
            .env("KORTTY_PACMAN_FILES", fs::canonicalize(source)?)
            .env("BUILDDIR", &work)
            .env("PKGDEST", &work)
            .env("SRCDEST", &work)
            .env("SRCPKGDEST", &work)
            .env("LOGDEST", &work)
            // package() copies the staged files and needs no application runtime libraries.
            .args(["--nodeps", "--nocolor", "--nosign", "--config"])
            .arg(config),
    )?;
    let name = format!("kortty-{version}-1-{arch}.pkg.tar.zst");
    let artifact = work.join(&name);
    let info = output(tool("pacman").arg("-Qip").arg(&artifact))?;
    let package_version = format!("{version}-1");
    for (field, expected) in [
        ("Name", "kortty"),
        ("Version", package_version.as_str()),
        ("Architecture", arch),
    ] {
        if !info.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(key, value)| key.trim() == field && value.trim() == expected)
        }) {
            return Err(invalid(format!("Pacman package has an unexpected {field}")).into());
        }
    }
    fs::rename(artifact, release.join(&name))?;
    Ok(name)
}

fn release_version(version: &str) -> Result<&str> {
    let number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'))
    };
    // Pacman versions order differently from SemVer. Keep the numeric release format used
    // by the Java package and avoid a conversion that changes precedence.
    if version.split('.').count() != 3 || !version.split('.').all(number) {
        return Err(invalid("Pacman requires a numeric major.minor.patch release version").into());
    }
    Ok(version)
}

fn elf_requirements(payload: &Path) -> Result<(String, String)> {
    let mut glibc = (0, 0, 0);
    let mut openssl = BTreeSet::new();
    for name in target::UNIX_PROGRAMS {
        let info = output(
            tool("readelf")
                .args(["--wide", "--dynamic", "--version-info"])
                .arg(payload.join("bin").join(name)),
        )?;
        let required = info
            .lines()
            .filter_map(|line| line.split_once("Name: GLIBC_"))
            .filter_map(|(_, version)| version.split_whitespace().next())
            .filter_map(glibc_version)
            .max()
            .ok_or_else(|| invalid(format!("{name} has no ELF glibc version requirement")))?;
        glibc = glibc.max(required);
        openssl.extend(info.lines().filter_map(openssl_dependency));
    }
    let (major, minor, patch) = glibc;
    let glibc = if patch == 0 {
        format!("{major}.{minor}")
    } else {
        format!("{major}.{minor}.{patch}")
    };
    let openssl = openssl.into_iter().collect::<Vec<_>>().join(" ");
    Ok((glibc, openssl))
}

fn glibc_version(version: &str) -> Option<(u32, u32, u32)> {
    if version == "ABI_DT_RELR" {
        return Some((2, 36, 0));
    }
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().map_or(Some(0), |value| value.parse().ok())?;
    parts.next().is_none().then_some((major, minor, patch))
}

fn openssl_dependency(line: &str) -> Option<String> {
    let (_, library) = line.split_once("(NEEDED)")?;
    let soname = library
        .trim()
        .strip_prefix("Shared library: [")?
        .strip_suffix(']')?;
    let (library, version) = soname.split_once(".so.")?;
    if !matches!(library, "libssl" | "libcrypto")
        || !version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    // Arch's virtual library provides distinguish the parallel OpenSSL ABIs.
    Some(format!("'{library}.so={version}-64'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_releases_preserve_numeric_upstream_versions() {
        for version in ["0.1.0", "2.26.0"] {
            assert_eq!(release_version(version).unwrap(), version);
        }
        for version in ["0.1", "0.01.0", "0.1.0-rc.1", "0.1.0+build.1"] {
            assert!(release_version(version).is_err());
        }
    }

    #[test]
    fn glibc_requirements_compare_each_numeric_component() {
        assert!(glibc_version("2.10") > glibc_version("2.9"));
        assert_eq!(glibc_version("2.2.5"), Some((2, 2, 5)));
        assert_eq!(glibc_version("ABI_DT_RELR"), Some((2, 36, 0)));
        assert_eq!(glibc_version("PRIVATE"), None);
    }

    #[test]
    fn pacman_tls_dependencies_follow_exact_needed_sonames() {
        let required = |version: &str| format!("0x1 (NEEDED)  Shared library: [{version}]");
        assert_eq!(
            openssl_dependency(&required("libssl.so.3")).as_deref(),
            Some("'libssl.so=3-64'")
        );
        assert_eq!(
            openssl_dependency(&required("libcrypto.so.4")).as_deref(),
            Some("'libcrypto.so=4-64'")
        );
        for unrelated in ["libc.so.6", "libssl.so", "libssl.so..3", "libssl.so.3'bad"] {
            assert!(openssl_dependency(&required(unrelated)).is_none());
        }
        assert!(openssl_dependency("0xe (SONAME) Library soname: [libssl.so.3]").is_none());
    }
}
