//! Packages the staged Windows executables with Windows Installer.

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, absolute};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use crate::target::{Platform, Target};
use crate::{Result, Staging, invalid};

const WIX_UI: &str = "WixToolset.UI.wixext";
const TEMPLATE: &str = include_str!("../windows/kortty.wxs");
const ICON: &[u8] = include_bytes!("../windows/kortty.ico");
const INSTALLER_HEADER: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

pub(crate) fn require_host(target: &Target, version: &str) -> Result<()> {
    if !cfg!(windows) || target.platform != Platform::Windows {
        return Err(invalid("--windows-msi requires a Windows build host and target").into());
    }
    validate_version(version)?;
    let output = run_wix(Command::new("wix.exe").arg("--version"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let major = text
        .trim()
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok());
    if !major.is_some_and(|major| major >= 4) {
        return Err(invalid("Windows MSI packaging requires WiX Toolset 4 or later").into());
    }
    Ok(())
}

pub(crate) fn write(
    triple: &str,
    version: &str,
    payload: &Path,
    release: &Path,
    stem: &str,
) -> Result<String> {
    let architecture = architecture(triple)?;
    let staging = wix_staging()?;
    let work = absolute(&staging.0)?;
    let source = work.join("kortty.wxs");
    fs::write(&source, authoring(payload, architecture)?)?;
    fs::write(work.join("kortty.ico"), ICON)?;
    let license = work.join("license.rtf");
    fs::write(
        &license,
        license_rtf(&fs::read_to_string(payload.join("license.txt"))?)?,
    )?;

    let name = format!("{stem}.msi");
    let installer = work.join("kortty.msi");
    run_wix(
        Command::new("wix.exe")
            .args(["build", "-arch", architecture, "-ext", WIX_UI, "-define"])
            .arg(format!("ReleaseVersion={version}"))
            .arg("-bindpath")
            .arg(path_argument(
                "kortty_payload=",
                &fs::canonicalize(payload)?,
            ))
            .arg("-bindpath")
            .arg(path_argument("kortty_resources=", &work))
            .arg("-bindvariable")
            .arg(path_argument("WixUILicenseRtf=", &license))
            .env("TEMP", &work)
            .env("TMP", &work)
            .arg("-intermediateFolder")
            .arg(&work)
            .args(["-pdbtype", "none", "-out"])
            .arg(&installer)
            .arg(&source),
    )?;

    let mut header = [0; INSTALLER_HEADER.len()];
    File::open(&installer)?.read_exact(&mut header)?;
    if header != INSTALLER_HEADER {
        return Err(invalid("WiX did not output a Windows Installer database").into());
    }
    fs::copy(installer, release.join(&name))?;
    Ok(name)
}

fn wix_staging() -> Result<Staging> {
    // WiX on .NET Framework reads its XML through a file URI, which cannot use a
    // verbatim Windows path. Reserve space for the private directory and WiX files.
    const MAX_BASE_LEN: usize = 160;
    let temp = absolute(env::temp_dir())?;
    let fits = |path: &Path| path.as_os_str().len() <= MAX_BASE_LEN;
    if fits(&temp) {
        return Staging::new(&temp);
    }
    let workspace = env::current_dir()?;
    let base = temp
        .ancestors()
        .take_while(|path| path.starts_with(&workspace))
        .find(|path| fits(path))
        .unwrap_or(&workspace);
    if !fits(base) {
        return Err(invalid(
            "WiX needs a shorter Windows TEMP directory for its XML and MSI staging",
        )
        .into());
    }
    Staging::new(base)
}

fn run_wix(command: &mut Command) -> Result<Output> {
    let output = command.output().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not start wix.exe (install WiX Toolset and {WIX_UI}): {error}"),
        )
    })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "WiX failed with {}:\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
        .into());
    }
    Ok(output)
}

fn validate_version(version: &str) -> Result<()> {
    let valid_component = |(component, max): (&str, u16)| {
        !component.is_empty()
            && component.bytes().all(|byte| byte.is_ascii_digit())
            && (component.len() == 1 || !component.starts_with('0'))
            && component.parse::<u16>().is_ok_and(|value| value <= max)
    };
    let components = version.split('.');
    if components.clone().count() == 3 && components.zip([255, 255, u16::MAX]).all(valid_component)
    {
        return Ok(());
    }
    Err(invalid("Windows MSI requires a major.minor.patch version within 255.255.65535").into())
}

fn architecture(triple: &str) -> Result<&'static str> {
    match triple.split_once('-').map(|(architecture, _)| architecture) {
        Some("x86_64") => Ok("x64"),
        Some("aarch64") => Ok("arm64"),
        _ => Err(invalid("Windows MSI requires an x64 or ARM64 target").into()),
    }
}

fn path_argument(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

fn authoring(payload: &Path, architecture: &str) -> Result<String> {
    let mut components = String::new();
    add_components(payload, "", architecture, &mut components)?;
    Ok(TEMPLATE.replace("@PAYLOAD_COMPONENTS@", components.trim_end()))
}

fn add_components(
    directory: &Path,
    relative_directory: &str,
    architecture: &str,
    components: &mut String,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        // Payload paths are Windows release filenames. Keep them safe in XML attributes and
        // opaque to the WiX variable parser.
        let name = name
            .to_str()
            .filter(|name| {
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            })
            .ok_or_else(|| invalid("a Windows MSI payload entry has an unsupported filename"))?;
        let relative_path = if relative_directory.is_empty() {
            name.to_owned()
        } else {
            format!("{relative_directory}\\{name}")
        };
        let entry_type = entry.file_type()?;
        if entry_type.is_dir() {
            add_components(&entry.path(), &relative_path, architecture, components)?;
        } else if entry_type.is_file() {
            add_file(components, &relative_path, architecture)?;
        } else {
            return Err(invalid("Windows MSI input contains a non-file payload entry").into());
        }
    }
    Ok(())
}

fn add_file(components: &mut String, relative_path: &str, architecture: &str) -> Result<()> {
    let (directory, file) = relative_path
        .rsplit_once('\\')
        .unwrap_or(("", relative_path));
    let (id, guid) = component_identity(architecture, relative_path);
    write!(
        components,
        r#"            <Component Id="Component_{id}" Guid="{guid}" Directory="INSTALLFOLDER""#
    )?;
    if !directory.is_empty() {
        write!(components, r#" Subdirectory="{directory}""#)?;
    }
    writeln!(components, ">")?;
    write!(
        components,
        r#"                <File Id="File_{id}" Name="{file}" Source="!(bindpath.kortty_payload){relative_path}" KeyPath="yes""#
    )?;
    if relative_path == "kortty.exe" {
        components.push_str(
            r#">
                    <Shortcut Id="KorttyStartMenu" Directory="ProgramMenuFolder"
                              Name="kortty Rust" Advertise="yes" Icon="KorttyIcon.exe" />
                    <Shortcut Id="KorttyDesktop" Directory="DesktopFolder"
                              Name="kortty Rust" Advertise="yes" Icon="KorttyIcon.exe" />
                </File>"#,
        );
        components.push('\n');
        writeln!(
            components,
            r#"                <RegistryValue Root="HKLM" Key="Software\kortty Rust"
                               Name="InstallFolder" Type="string" Value="[INSTALLFOLDER]" />"#
        )?;
    } else {
        writeln!(components, " />")?;
    }
    writeln!(components, "            </Component>")?;
    Ok(())
}

fn component_identity(architecture: &str, path: &str) -> (String, String) {
    // Each file is a component. Keep its key stable across releases of one architecture and
    // distinct when an ARM64 installer replaces the x64 edition or vice versa.
    let digest = Sha256::digest(format!(
        "kortty/windows-msi/component/v1\0{architecture}\0{}",
        path.to_ascii_lowercase()
    ));
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("truncated SHA-256 digest");
    let id = format!("{:032x}", u128::from_be_bytes(bytes));
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    let guid = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..],
    );
    (id, guid)
}

fn license_rtf(text: &str) -> Result<String> {
    let mut rtf = String::from(r"{\rtf1\ansi\uc1{\fonttbl{\f0 Segoe UI;}}\f0\fs20 ");
    for line in text.lines() {
        for unit in line.encode_utf16() {
            match unit {
                0x5c | 0x7b | 0x7d => {
                    rtf.push('\\');
                    rtf.push(char::from(unit as u8));
                }
                0x20..=0x7e => rtf.push(char::from(unit as u8)),
                _ => write!(rtf, "\\u{}?", unit as i16)?,
            }
        }
        rtf.push_str("\\par\n");
    }
    rtf.push('}');
    Ok(rtf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_msi_versions_fit_the_windows_installer_fields() {
        for version in ["0.1.0", "255.255.65535"] {
            validate_version(version).unwrap();
        }
        for version in [
            "1",
            "1.2",
            "256.0.0",
            "2.256.0",
            "3.0.65536",
            "3.0.0.1",
            "1.00.0",
            "1.0.0-rc.1",
        ] {
            assert!(validate_version(version).is_err(), "{version}");
        }
    }

    #[test]
    fn component_ids_are_stable_and_scoped_to_the_file_and_architecture() {
        let app = component_identity("arm64", "kortty.exe");
        assert_eq!(app, component_identity("arm64", "KORTTY.exe"));
        assert_ne!(app, component_identity("x64", "kortty.exe"));
        assert_ne!(app, component_identity("arm64", "kortty-cli.exe"));
        assert_eq!(architecture("aarch64-pc-windows-msvc").unwrap(), "arm64");
        assert_eq!(architecture("x86_64-pc-windows-msvc").unwrap(), "x64");
    }

    #[test]
    fn msi_has_shortcuts_only_for_the_app_and_preserves_license_subdirectories() {
        let mut components = String::new();
        add_file(&mut components, "licenses\\latex-rust\\notice.txt", "arm64").unwrap();
        add_file(&mut components, "kortty-cli.exe", "arm64").unwrap();
        add_file(&mut components, "kortty-ssh-worker.exe", "arm64").unwrap();
        assert!(components.contains(r#"Subdirectory="licenses\latex-rust""#));
        assert!(components.contains(r#"Name="kortty-ssh-worker.exe""#));
        assert!(!components.contains("<Shortcut"));
        add_file(&mut components, "kortty.exe", "arm64").unwrap();
        assert_eq!(components.matches("<Shortcut ").count(), 2);
    }

    #[test]
    fn license_rtf_escapes_rich_text_syntax_and_preserves_unicode() {
        let license = license_rtf("MIT License\r\n{Example}\\ö").unwrap();
        assert!(license.contains("MIT License\\par\n"));
        assert!(license.contains(r"\{Example\}\\\u246?"));
    }
}
