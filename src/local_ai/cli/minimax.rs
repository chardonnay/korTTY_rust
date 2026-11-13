//! An explicitly enabled MiniMax text CLI with pinned package code and HTTPS routing.

use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{Argument, Error, MAX_INPUT, Mode, Program, active};

const BOOTSTRAP: &str = include_str!("minimax.mjs");
const MAX_MODULE: u64 = 512 * 1024;
const MAX_CONFIG: u64 = 32 * 1024;
const MAX_WIRE: usize = 3 * MAX_INPUT;

// The published npm dist/mmx.mjs SHA-256 values. The module loader exposes
// virtual config, messages and disabled subprocess/proxy hooks to these bytes.
const RELEASES: &[(&str, &str)] = &[
    (
        "1.0.15",
        "a65073ddfcaf3c83cb826392dd7ca3f0cfde80016ddf0e26bab45117fb12f72c",
    ),
    (
        "1.0.29",
        "f70cc9e43e141192fcdb69eb80577b15fcfac7a6c5a8341e90ca6d2ad1fc0522",
    ),
];

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MiniMaxRegion {
    #[default]
    Global,
    China,
}

impl MiniMaxRegion {
    pub(crate) const ALL: [Self; 2] = [Self::Global, Self::China];

    const fn id(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::China => "cn",
        }
    }

    fn base(self, version: &str) -> &'static str {
        match self {
            Self::Global => "https://api.minimax.io",
            Self::China if version == "1.0.15" => "https://api.minimaxi.com",
            Self::China => "https://api.minimax.cn",
        }
    }
}

impl fmt::Display for MiniMaxRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Global => "Global",
            Self::China => "China",
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct MiniMax {
    module: PathBuf,
    package: &'static str,
    version: &'static str,
    config: PathBuf,
    region: MiniMaxRegion,
    model: String,
}

impl Program {
    pub(crate) fn inspect_minimax_module(path: &Path) -> Result<(PathBuf, String), String> {
        let path = path.canonicalize().map_err(|_| module_changed())?;
        let bytes = read(&path, "mmx.mjs", MAX_MODULE, false).map_err(|_| module_changed())?;
        let hash = digest(&bytes).map_err(|_| module_changed())?;
        release(&hash)?;
        Ok((path, hash))
    }

    pub(crate) fn approved_minimax(
        node: PathBuf,
        fingerprint: [u8; 32],
        module: PathBuf,
        package: &str,
        config: PathBuf,
        region: MiniMaxRegion,
        model: &str,
    ) -> Result<Self, String> {
        let (version, package) = release(package)?;
        if !crate::private_files::executable::valid_path(&node)
            || !valid_path(&module, "mmx.mjs")
            || !valid_path(&config, "config.json")
            || !model.is_empty() && (!super::super::valid_model(model) || model.starts_with('-'))
        {
            return Err("Enter absolute Node.js, MiniMax package and login config paths, and a valid optional model.".into());
        }
        Ok(Self {
            path: node,
            arguments: [
                "--use-system-ca",
                "--input-type=module",
                "--eval",
                BOOTSTRAP,
            ]
            .into_iter()
            .map(|argument| Argument::Literal(argument.into()))
            .collect(),
            mode: Mode::MiniMax,
            fingerprint,
            codex_preset: false,
            minimax: Some(Box::new(MiniMax {
                module,
                package,
                version,
                config,
                region,
                model: model.into(),
            })),
        })
    }
}

impl MiniMax {
    pub(super) fn destination(&self) -> String {
        format!("{}/anthropic/v1/messages", self.region.base(self.version))
    }

    pub(super) fn review(&self) -> String {
        format!(
            "\nMiniMax package: {} (mmx-cli {})\nPackage SHA-256: {}\nLogin config: {}\nHTTPS destination: {}\nCommand: text chat · {} · model {} · JSON stdin · text output · redirects blocked",
            self.module.display(),
            self.version,
            self.package,
            self.config.display(),
            self.destination(),
            self.region,
            self.model(),
        )
    }

    fn model(&self) -> &str {
        if !self.model.is_empty() {
            &self.model
        } else if self.version == "1.0.15" {
            "MiniMax-M2.7"
        } else {
            "MiniMax-M3.1-Flash"
        }
    }

    pub(super) fn prepare(
        &self,
        messages: &str,
        deadline: Instant,
        canceled: &AtomicBool,
    ) -> Result<Invocation, Error> {
        let module = read(&self.module, "mmx.mjs", MAX_MODULE, false)
            .map_err(|_| Error::Review(module_changed()))?;
        if digest(&module).map_err(|_| Error::Service)? != self.package {
            return Err(Error::Review(module_changed()));
        }
        active(deadline, canceled)?;
        let key = self.read_key()?;
        require_no_login_in_prompt(messages, &key)?;
        let module = STANDARD.encode(&module);
        let wire = Zeroizing::new(
            serde_json::to_string(&Envelope {
                module: &module,
                key: &key,
                messages,
                region: self.region.id(),
                base: self.region.base(self.version),
                model: self.model(),
                timeout: deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1),
            })
            .map_err(|_| Error::Service)?,
        );
        if wire.len() > MAX_WIRE {
            return Err(Error::Invalid(
                "The MiniMax CLI request exceeds its input limit.",
            ));
        }
        Ok(Invocation { wire, key })
    }

    pub(super) fn require_current_key(&self, expected: &str) -> Result<(), Error> {
        if self.read_key()?.as_str() != expected {
            return Err(Error::Review(
                "The MiniMax login changed before dispatch. Submit a new request.".into(),
            ));
        }
        Ok(())
    }

    fn read_key(&self) -> Result<Zeroizing<String>, Error> {
        let login_error = || {
            Error::Review(
            "Could not read a private MiniMax login. Run mmx auth login and verify the selected region and config path.".into(),
        )
        };
        let bytes =
            read(&self.config, "config.json", MAX_CONFIG, true).map_err(|_| login_error())?;
        let login: Login<'_> = serde_json::from_slice(&bytes).map_err(|_| login_error())?;
        if login
            .region
            .is_some_and(|region| region != self.region.id())
            || login
                .base_url
                .is_some_and(|url| url.trim_end_matches('/') != self.region.base(self.version))
        {
            return Err(Error::Review("The MiniMax login region or endpoint differs from this profile. Review the matching region before sending.".into()));
        }
        let key = if let Some(oauth) = login.oauth {
            if oauth
                .region
                .is_some_and(|region| region != self.region.id())
                || oauth
                    .resource_url
                    .is_some_and(|url| url.trim_end_matches('/') != self.region.base(self.version))
            {
                return Err(Error::Review(
                    "The MiniMax OAuth region or endpoint differs from this profile.".into(),
                ));
            }
            if !oauth
                .expires_at
                .parse::<jiff::Timestamp>()
                .is_ok_and(|expiry| expiry > jiff::Timestamp::now())
            {
                return Err(Error::Review(
                    "Refresh the MiniMax login with mmx auth refresh before using this profile."
                        .into(),
                ));
            }
            oauth.access_token
        } else {
            login.api_key.ok_or_else(login_error)?
        };
        super::super::require_credential_value(key)?;
        Ok(Zeroizing::new(key.into()))
    }
}

fn require_no_login_in_prompt(messages: &str, key: &str) -> Result<(), Error> {
    if super::super::credential_echo(messages, key) {
        return Err(Error::Review(
            "The MiniMax prompt contains its login credential. Remove it before sending.".into(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct Envelope<'a> {
    module: &'a str,
    key: &'a str,
    messages: &'a str,
    region: &'a str,
    base: &'a str,
    model: &'a str,
    timeout: u64,
}

#[derive(Deserialize)]
struct Login<'a> {
    api_key: Option<&'a str>,
    region: Option<&'a str>,
    base_url: Option<&'a str>,
    #[serde(borrow)]
    oauth: Option<OAuth<'a>>,
}

#[derive(Deserialize)]
struct OAuth<'a> {
    access_token: &'a str,
    expires_at: &'a str,
    region: Option<&'a str>,
    resource_url: Option<&'a str>,
}

pub(super) struct Invocation {
    pub(super) wire: Zeroizing<String>,
    pub(super) key: Zeroizing<String>,
}

pub(crate) fn valid_minimax_module_path(path: &Path) -> bool {
    valid_path(path, "mmx.mjs")
}

pub(crate) fn valid_minimax_config_path(path: &Path) -> bool {
    valid_path(path, "config.json")
}

fn valid_path(path: &Path, name: &str) -> bool {
    use std::path::Component;
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        || path.components().any(|part| matches!(part, Component::Normal(name) if name.to_str().is_some_and(|name| name.contains(':'))))
    {
        return false;
    }
    path.is_absolute()
        && path.file_name().is_some_and(|actual| actual == name)
        && path.components().count() <= 64
        && !path.components().any(|part| part == Component::ParentDir)
        && path
            .to_str()
            .is_some_and(|name| name.len() <= 4096 && !name.chars().any(char::is_control))
}

fn read(path: &Path, name: &str, limit: u64, secret: bool) -> io::Result<Zeroizing<Vec<u8>>> {
    if !valid_path(path, name) {
        return Err(io::Error::other(
            "Invalid MiniMax package or configuration path",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing MiniMax parent directory"))?;
    #[cfg(unix)]
    let directory = Directory::open_owner_controlled(parent)?;
    #[cfg(windows)]
    let directory = Directory::open(parent, false)?;
    // Unix login files are mode 0600. On Windows, the official CLI uses the
    // inherited user profile ACL rather than kortty's exact single-ACE DACL.
    #[cfg(unix)]
    let file = if secret {
        directory.open_private_file(name)?
    } else {
        directory.open_file(name)?
    };
    #[cfg(windows)]
    let file = directory.open_file(name)?;
    if secret {
        require_single_link(&file)?;
        #[cfg(windows)]
        kortty_files::tree::Directory::require_user_credential_file(&file)?;
    }
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(limit + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "MiniMax package or configuration exceeds its file limit",
        ));
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> io::Result<String> {
    use std::fmt::Write as _;
    let hash = HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?;
    let mut hex = String::with_capacity(64);
    for byte in hash {
        write!(hex, "{byte:02x}").expect("writing to a string");
    }
    Ok(hex)
}

fn release(package: &str) -> Result<(&'static str, &'static str), String> {
    RELEASES
        .iter()
        .copied()
        .find(|(_, hash)| *hash == package)
        .ok_or_else(module_changed)
}

fn module_changed() -> String {
    "Choose the unmodified official mmx-cli 1.0.15 or 1.0.29 dist/mmx.mjs file and review it again."
        .into()
}

#[cfg(test)]
mod tests;
