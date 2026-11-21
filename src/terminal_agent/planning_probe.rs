//! Fixed, bounded environment evidence for the advisory Planning workflow.

use serde_json::{Map, Value};

use super::{Outcome, Status, Target};

const POSIX: &str = r#"field() { printf '%s=%s\n' "$1" "$(printf '%s' "$2" | tr '\r\n\t' '   ' | cut -c 1-2000)"; }
os=$(awk -F= '/^PRETTY_NAME=/ { sub(/^[^=]*=/, ""); gsub(/^"|"$/, ""); print; exit }' /etc/os-release 2>/dev/null)
if [ -z "$os" ] && command -v sw_vers >/dev/null 2>&1; then os="$(sw_vers -productName) $(sw_vers -productVersion)"; fi
field osRelease "${os:-$(uname -s)}"
field kernel "$(uname -sr)"
field architecture "$(uname -m)"
field shell "${SHELL:-/bin/sh}"
field currentUser "$(id -un)"
uid=$(id -u)
field uid "$uid"
field gid "$(id -g)"
field groups "$(id -nG | tr ' ' ',')"
field homeDir "${HOME:-}"
field currentDir "$(pwd -P)"
disk=$(df -Pk . 2>/dev/null)
field availableDiskKb "$(printf '%s\n' "$disk" | awk 'NR==2 { print $4 }')"
field availableDiskPath "$(printf '%s\n' "$disk" | awk 'NR==2 { $1=$2=$3=$4=$5=""; sub(/^ +/, ""); print }')"
available() { for tool do if command -v "$tool" >/dev/null 2>&1; then printf '%s,' "$tool"; fi; done; }
field packageManagers "$(available apt apt-get dnf yum zypper pacman apk brew)"
field serviceManagers "$(available systemctl service rc-service launchctl)"
root=false; has_sudo=false; noninteractive=false; escalation=none; listing=
if [ "$uid" = 0 ]; then root=true; escalation=already_root; fi
if command -v sudo >/dev/null 2>&1; then
  has_sudo=true
  listing=$(sudo -n -l 2>&1 | tail -c 2000)
  if [ "$root" = false ]; then
    escalation=sudo_password
    if sudo -k -n true >/dev/null 2>&1; then noninteractive=true; escalation=passwordless_sudo; fi
  fi
fi
field alreadyRoot "$root"
field sudoAvailable "$has_sudo"
field passwordlessSudo "$noninteractive"
field sudoNonInteractive "$noninteractive"
field sudoNListSummary "$listing"
field rootEscalationMode "$escalation""#;

#[cfg(windows)]
const POWERSHELL: &str = r#"function Field($key, $value) { '{0}={1}' -f $key, ([string]$value -replace '[\r\n\t]', ' ') }
Field osRelease ('Windows ' + [Environment]::OSVersion.VersionString)
Field kernel ([Environment]::OSVersion.Version)
Field architecture ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)
Field shell 'PowerShell'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
Field currentUser $identity.Name
Field uid $identity.User.Value
Field groups (($identity.Groups | ForEach-Object { $_.Value }) -join ',')
Field homeDir $env:USERPROFILE
$location = Get-Location
Field currentDir $location.ProviderPath
if ($location.Drive.Free -ne $null) { Field availableDiskKb ([long][Math]::Floor($location.Drive.Free / 1KB)); Field availableDiskPath $location.Drive.Root }
Field packageManagers ((Get-Command winget,choco,scoop -ErrorAction SilentlyContinue | ForEach-Object { $_.Name }) -join ',')
Field serviceManagers 'Get-Service,sc.exe'
$admin = (New-Object Security.Principal.WindowsPrincipal($identity)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Field alreadyRoot (([string]$admin).ToLowerInvariant())"#;

pub(crate) fn command(target: &Target) -> &'static str {
    #[cfg(windows)]
    if let Target::Local { shell, .. } = target {
        match shell {
            super::Shell::PowerShell(_) => return POWERSHELL,
            super::Shell::CommandPrompt(_) => return target.probe_command(),
        }
    }
    let _ = target;
    POSIX
}

pub(crate) struct Snapshot(Value);

impl Snapshot {
    pub(crate) fn capture(
        target: &Target,
        outcome: &Outcome,
        mask: &impl Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        if outcome.status != Status::Exited(0) || outcome.stdout.truncated() {
            return Err("The planning environment probe did not produce a complete result.".into());
        }
        let stdout = outcome.stdout.text();
        #[cfg(windows)]
        if matches!(
            target,
            Target::Local {
                shell: super::Shell::CommandPrompt(_),
                ..
            }
        ) {
            return cmd_snapshot(&stdout, mask);
        }
        let _ = target;
        Self::parse(&stdout, mask)
    }

    fn parse(stdout: &str, mask: &impl Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let mut fields = Map::new();
        for (key, value) in stdout.lines().filter_map(|line| line.split_once('=')) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let field = match key {
                "osRelease" | "kernel" | "architecture" | "shell" | "currentUser" | "uid"
                | "gid" | "homeDir" | "currentDir" | "availableDiskPath" | "sudoNListSummary"
                | "rootEscalationMode" => bounded_text(value, mask)?.into(),
                "availableDiskKb" => match value.parse::<u64>() {
                    Ok(size) => size.into(),
                    Err(_) => continue,
                },
                "groups" | "packageManagers" | "serviceManagers" => Value::Array(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .take(64)
                        .map(|part| bounded_text(part, mask).map(Value::String))
                        .collect::<Result<_, _>>()?,
                ),
                "alreadyRoot" | "sudoAvailable" | "passwordlessSudo" | "sudoNonInteractive" => {
                    match value {
                        "true" => true.into(),
                        "false" => false.into(),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            if fields.insert(key.into(), field).is_some() {
                return Err("The planning environment probe returned duplicate fields.".into());
            }
        }
        if !fields.contains_key("osRelease") || !fields.contains_key("currentDir") {
            return Err(
                "The planning environment probe returned incomplete system or directory evidence."
                    .into(),
            );
        }
        Ok(Self(fields.into()))
    }

    pub(crate) fn input(&self) -> &Value {
        &self.0
    }

    pub(crate) fn needs_password_sudo(&self) -> bool {
        self.has_sudo_as_non_root() && self.0["passwordlessSudo"] == false
    }

    pub(crate) fn has_sudo_as_non_root(&self) -> bool {
        self.0["alreadyRoot"] == false && self.0["sudoAvailable"] == true
    }

    pub(crate) fn summary(&self) -> String {
        let field = |key| self.0[key].as_str().unwrap_or("unknown");
        let mut summary = format!(
            "{} · user {} · cwd {}",
            field("osRelease"),
            field("currentUser"),
            field("currentDir")
        );
        let capability = if self.0["alreadyRoot"] == true {
            Some("administrator or root")
        } else if self.0["passwordlessSudo"] == true {
            Some("noninteractive sudo verified")
        } else if self.0["sudoAvailable"] == true {
            Some("sudo available")
        } else if self.0["sudoAvailable"] == false {
            Some("no sudo")
        } else {
            None
        };
        if let Some(capability) = capability {
            summary.push_str(" · ");
            summary.push_str(capability);
        }
        summary
    }
}

fn bounded_text(
    value: &str,
    mask: &impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let value = mask(value)?;
    Ok(value.chars().take(2000).collect())
}

#[cfg(any(windows, test))]
fn cmd_snapshot(
    stdout: &str,
    mask: &impl Fn(&str) -> Result<String, String>,
) -> Result<Snapshot, String> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let (Some(os), Some(user), Some(directory)) = (lines.next(), lines.next(), lines.next()) else {
        return Err(
            "The Windows planning probe returned incomplete system or directory evidence.".into(),
        );
    };
    if lines.next().is_some() {
        return Err("The Windows planning probe returned unexpected output.".into());
    }
    Snapshot::parse(
        &format!("osRelease={os}\ncurrentUser={user}\ncurrentDir={directory}\nshell=CMD"),
        mask,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn planning_probe_parses_only_bounded_typed_environment_fields() {
        let snapshot = Snapshot::parse(
            "osRelease=Linux\ncurrentUser=fixture\ncurrentDir=/work\nuid=1000\navailableDiskKb=128\npackageManagers=apt, system-tool,\nsudoAvailable=true\npasswordlessSudo=false\nsudoNListSummary=secret\nunexpected=unreviewed\n",
            &|text| Ok(text.replace("secret", "***")),
        ).unwrap();
        assert_eq!(snapshot.input()["availableDiskKb"], 128);
        assert_eq!(
            snapshot.input()["packageManagers"],
            json!(["apt", "system-tool"])
        );
        assert_eq!(snapshot.input()["sudoNListSummary"], "***");
        assert!(snapshot.input().get("unexpected").is_none());
        assert_eq!(
            snapshot.summary(),
            "Linux · user fixture · cwd /work · sudo available"
        );
        assert!(
            Snapshot::parse(
                "osRelease=Linux\nosRelease=Changed\ncurrentDir=/work",
                &|text| Ok(text.into())
            )
            .is_err()
        );
    }

    #[test]
    fn planning_probe_maps_the_native_cmd_output_without_guessing_capabilities() {
        let snapshot = cmd_snapshot(
            "\r\nMicrosoft Windows [Version 10.0.26100]\r\nuser\\fixture\r\nC:\\work\r\n",
            &|text| Ok(text.into()),
        )
        .unwrap();
        assert_eq!(snapshot.input()["shell"], "CMD");
        assert_eq!(snapshot.input()["currentDir"], r"C:\work");
        assert!(snapshot.input().get("sudoAvailable").is_none());
    }

    #[test]
    fn planning_sudo_preflight_requires_all_three_capability_fields() {
        for (root, available, passwordless, expected) in [
            ("false", "true", "false", true),
            ("true", "true", "false", false),
            ("false", "false", "false", false),
            ("false", "true", "true", false),
            ("false", "true", "", false),
        ] {
            let snapshot = Snapshot::parse(
                &format!(
                    "osRelease=Linux\ncurrentDir=/work\nalreadyRoot={root}\nsudoAvailable={available}\npasswordlessSudo={passwordless}"
                ),
                &|text| Ok(text.into()),
            )
            .unwrap();
            assert_eq!(snapshot.needs_password_sudo(), expected);
        }
    }
}
