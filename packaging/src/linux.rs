//! Linux package builders run only when explicitly requested.

use std::fs::{self, File, FileTimes};
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use crate::{Result, invalid, target};

// A fixed epoch keeps build-time clocks and input file mtimes out of the packages.
const EPOCH: u64 = 1_767_225_600;
const MAINTAINER: &str = "korTTY maintainers <chardonnay@users.noreply.github.com>";
const DEB_RUNTIME: &str = "ca-certificates, fontconfig, fonts-dejavu-mono | fonts-dejavu-core, libwayland-client0, libwayland-cursor0, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, libxrandr2, xkb-data";
const RPM_RUNTIME: &str = "ca-certificates, fontconfig, dejavu-sans-mono-fonts, libwayland-client.so.0()(64bit), libwayland-cursor.so.0()(64bit), libX11.so.6()(64bit), libX11-xcb.so.1()(64bit), libxcb.so.1()(64bit), libXcursor.so.1()(64bit), libXi.so.6()(64bit), libxkbcommon.so.0()(64bit), libxkbcommon-x11.so.0()(64bit), libXrandr.so.2()(64bit), xkeyboard-config";

#[derive(Default)]
pub(crate) struct Packages {
    pub(crate) zip: bool,
    pub(crate) deb: bool,
    pub(crate) rpm: bool,
}

impl Packages {
    pub(crate) fn selected(&self) -> bool {
        self.zip || self.deb || self.rpm
    }

    pub(crate) fn option(&self) -> &'static str {
        if self.deb {
            "--linux-deb"
        } else if self.rpm {
            "--linux-rpm"
        } else {
            "--linux-zip"
        }
    }

    pub(crate) fn require_host(&self, triple: &str) -> Result<()> {
        if self.deb || self.rpm {
            if !cfg!(target_os = "linux") || target::native()? != triple {
                return Err(
                    invalid("Native Linux packages require a matching Linux build host").into(),
                );
            }
            if self.deb {
                let (_, architecture) = architectures(triple)?;
                let actual = output(tool("dpkg").arg("--print-architecture"))?;
                if actual.trim() != architecture {
                    return Err(invalid(
                        "The Debian package architecture differs from the build host",
                    )
                    .into());
                }
                output(tool("dpkg-deb").arg("--version"))?;
                output(tool("dpkg-shlibdeps").arg("--version"))?;
            }
            if self.rpm {
                output(tool("rpmbuild").arg("--version"))?;
                output(tool("rpm").arg("--version"))?;
            }
        }
        Ok(())
    }

    pub(crate) fn write(
        &self,
        triple: &str,
        version: &str,
        payload: &Path,
        staging: &Path,
        release: &Path,
    ) -> Result<Vec<String>> {
        if !self.deb && !self.rpm {
            return Ok(Vec::new());
        }
        let (rpm_arch, deb_arch) = architectures(triple)?;
        let (deb_version, rpm_version) = versions(version)?;
        let mut artifacts = Vec::new();
        if self.deb {
            artifacts.push(build_deb(
                staging,
                release,
                payload,
                &deb_version,
                deb_arch,
            )?);
        }
        if self.rpm {
            artifacts.push(build_rpm(
                staging,
                release,
                payload,
                &rpm_version,
                rpm_arch,
            )?);
        }
        Ok(artifacts)
    }
}

fn architectures(triple: &str) -> Result<(&'static str, &'static str)> {
    match triple {
        "aarch64-unknown-linux-gnu" => Ok(("aarch64", "arm64")),
        "x86_64-unknown-linux-gnu" => Ok(("x86_64", "amd64")),
        _ => Err(invalid("Unsupported Linux package architecture").into()),
    }
}

fn versions(version: &str) -> Result<(String, String)> {
    let (version, metadata) = version
        .split_once('+')
        .map_or((version, None), |(version, metadata)| {
            (version, Some(metadata))
        });
    let (base, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(base, prerelease)| {
            (base, Some(prerelease))
        });
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let identifiers = |value: &str| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    };
    if base.split('.').count() != 3
        || !base.split('.').all(digits)
        || prerelease.is_some_and(|part| !identifiers(part))
        || metadata.is_some_and(|part| !identifiers(part))
    {
        return Err(invalid("Invalid Linux package version").into());
    }
    let mut deb = base.to_owned();
    let mut rpm = base.to_owned();
    if let Some(prerelease) = prerelease {
        deb.push('~');
        deb.push_str(prerelease);
        rpm.push('~');
        rpm.push_str(&prerelease.replace('-', "_"));
    }
    if let Some(metadata) = metadata {
        deb.push('+');
        deb.push_str(metadata);
        rpm.push('+');
        rpm.push_str(&metadata.replace('-', "_"));
    }
    Ok((deb, rpm))
}

fn build_deb(
    staging: &Path,
    release: &Path,
    payload: &Path,
    version: &str,
    arch: &str,
) -> Result<String> {
    let work = staging.join("debian-build");
    let package = work.join("package");
    let installed_kib = stage_install(payload, &package, false)?;
    let debian = work.join("debian");
    fs::create_dir(&debian)?;
    fs::write(
        debian.join("control"),
        format!(
            "Source: kortty\nSection: utils\nPriority: optional\nMaintainer: {MAINTAINER}\n\nPackage: kortty\nArchitecture: any\nDescription: Native terminal emulator and SSH client\n"
        ),
    )?;
    let calculated = output(tool("dpkg-shlibdeps").current_dir(&work).args([
        "-O",
        "-e",
        "package/usr/bin/kortty",
        "-e",
        "package/usr/bin/kortty-cli",
    ]))?;
    let dependencies = calculated
        .trim()
        .strip_prefix("shlibs:Depends=")
        .filter(|value| !value.is_empty() && !value.contains(['\r', '\n']))
        .ok_or_else(|| invalid("dpkg-shlibdeps did not report shared library dependencies"))?;
    let control = package.join("DEBIAN");
    fs::create_dir(&control)?;
    fs::write(
        control.join("control"),
        format!(
            "Package: kortty\nVersion: {version}-1\nSection: utils\nPriority: optional\nArchitecture: {arch}\nMaintainer: {MAINTAINER}\nInstalled-Size: {installed_kib}\nDepends: {dependencies}, {DEB_RUNTIME}\nDescription: Native terminal emulator and SSH client\nHomepage: https://github.com/chardonnay/korTTY_rust\n"
        ),
    )?;
    normalize(&package)?;
    let name = format!("kortty_{version}-1_{arch}.deb");
    output(
        tool("dpkg-deb")
            .args(["--root-owner-group", "-Zgzip", "--build"])
            .arg(&package)
            .arg(release.join(&name)),
    )?;
    Ok(name)
}

fn build_rpm(
    staging: &Path,
    release: &Path,
    payload: &Path,
    version: &str,
    arch: &str,
) -> Result<String> {
    let work = staging.join("rpm-build");
    let buildroot = work.join("root");
    stage_install(payload, &buildroot, true)?;
    let spec = work.join("kortty.spec");
    fs::write(
        &spec,
        format!(
            r#"Name: kortty
Version: {version}
Release: 1
Summary: Native terminal emulator and SSH client
License: MIT
URL: https://github.com/chardonnay/korTTY_rust
Requires: {RPM_RUNTIME}

%description
Native terminal emulator and SSH client.

%install
mkdir -p "$RPM_BUILD_ROOT"
cp -a "$KORTTY_RPM_SOURCE/." "$RPM_BUILD_ROOT/"

%files
%defattr(-,root,root,-)
/usr/bin/kortty
/usr/bin/kortty-cli
/usr/share/applications/kortty.desktop
/usr/share/pixmaps/kortty.png
%license /usr/share/licenses/kortty
"#
        ),
    )?;
    let work = fs::canonicalize(work)?;
    let package = work.join("out");
    let temporary = work.join("tmp");
    fs::create_dir(&package)?;
    fs::create_dir(&temporary)?;
    let definition = |name, path: &Path| -> Result<String> {
        let path = path
            .to_str()
            .ok_or_else(|| invalid("RPM build path is not UTF-8"))?;
        // RPM inserts these values into the build shell template.
        if !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" /_.-".contains(&byte))
        {
            return Err(invalid("RPM build path contains unsupported characters").into());
        }
        Ok(format!("{name} {path}"))
    };
    output(
        tool("rpmbuild")
            .current_dir(&work)
            .env("KORTTY_RPM_SOURCE", fs::canonicalize(buildroot)?)
            .args(["-bb", "--target", arch])
            .args([
                "--define",
                &definition("_topdir", &work)?,
                "--define",
                &definition("_rpmdir", &package)?,
                "--define",
                &definition("_tmppath", &temporary)?,
            ])
            .args([
                "--define",
                "_build_name_fmt %{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}.rpm",
            ])
            .args([
                "--define",
                "debug_package %{nil}",
                "--define",
                "__os_install_post %{nil}",
            ])
            .args([
                "--define",
                "_buildhost kortty",
                "--define",
                "use_source_date_epoch_as_buildtime 1",
                "--define",
                "source_date_epoch_from_changelog 0",
            ])
            .arg("kortty.spec"),
    )?;
    let name = format!("kortty-{version}-1.{arch}.rpm");
    let artifact = package.join(&name);
    let dependencies = output(tool("rpm").args(["-qp", "--requires"]).arg(&artifact))?;
    if !dependencies
        .lines()
        .any(|line| line.starts_with("libc.so.6(GLIBC_"))
    {
        return Err(
            invalid("RPM ELF dependency generation did not report a glibc requirement").into(),
        );
    }
    fs::rename(artifact, release.join(&name))?;
    Ok(name)
}

fn stage_install(payload: &Path, package: &Path, rpm: bool) -> Result<u64> {
    let usr = package.join("usr");
    copy_tree(&payload.join("bin"), &usr.join("bin"))?;
    copy_tree(&payload.join("share"), &usr.join("share"))?;
    let license = usr.join(if rpm {
        "share/licenses/kortty"
    } else {
        "share/doc/kortty"
    });
    copy_tree(&payload.join("licenses"), &license.join("third-party"))?;
    fs::copy(
        payload.join("license.txt"),
        license.join(if rpm { "LICENSE" } else { "copyright" }),
    )?;
    let bytes = normalize(package)?;
    Ok(bytes.div_ceil(1024))
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    let mut paths = fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
    paths.sort_by_key(fs::DirEntry::file_name);
    for entry in paths {
        let kind = entry.file_type()?;
        let output = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &output)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), output)?;
        } else {
            return Err(invalid("Unexpected entry in Linux package staging"));
        }
    }
    Ok(())
}

fn normalize(path: &Path) -> io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    let bytes = if metadata.is_dir() {
        let mut bytes = 0;
        for child in fs::read_dir(path)? {
            bytes += normalize(&child?.path())?;
        }
        bytes
    } else if metadata.is_file() {
        metadata.len()
    } else {
        return Err(invalid("Unexpected entry in Linux package staging"));
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "bin");
        let mode = if metadata.is_dir() || executable {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    File::open(path)?
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(EPOCH)))?;
    Ok(bytes)
}

fn tool(name: &str) -> Command {
    let mut command = Command::new(name);
    command
        .env("LC_ALL", "C")
        .env("SOURCE_DATE_EPOCH", EPOCH.to_string());
    command
}

fn output(command: &mut Command) -> Result<String> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| io::Error::new(error.kind(), format!("{name}: {error}")))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
