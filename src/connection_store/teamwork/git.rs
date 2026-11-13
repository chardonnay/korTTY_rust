//! Read-only Git transport for a bounded Teamwork connection snapshot.

use std::io;
use std::str;
use std::sync::atomic::AtomicBool;

use url::{Host, Url};
use zeroize::Zeroizing;

use crate::ssh_dialog::{normalize_host, unsafe_text};

use super::super::import::{MAX_TEAMWORK_BYTES, TeamworkEntry, read_teamwork_bytes};
use super::{MAX_LOCATION_BYTES, invalid};

mod process;
use process::Git;

const PRIMARY: &str = "kortty-teamwork-connections.xml";
const LEGACY: &str = "connections.xml";
const TRACKING: &str = "refs/kortty/teamwork";
const MAX_REFS_OUTPUT: usize = 8 * 1024;
const MAX_BRANCH_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GitUrl(String);

impl GitUrl {
    pub(crate) fn parse(input: &str) -> io::Result<Self> {
        let input = input.trim();
        let bad_url =
            || invalid("Enter an HTTP, HTTPS, SSH or git:// repository URL without credentials.");
        if input.len() > MAX_LOCATION_BYTES
            || input.starts_with('-')
            || input.contains('\\')
            || input.chars().any(char::is_whitespace)
            || unsafe_text(input)
        {
            return Err(bad_url());
        }
        let (_, location) = input.split_once("://").ok_or_else(bad_url)?;
        let (authority, path) = location.split_once('/').ok_or_else(bad_url)?;
        if authority.is_empty() || path.is_empty() || authority.matches('@').count() > 1 {
            return Err(bad_url());
        }
        let mut url = Url::parse(input).map_err(|_| bad_url())?;
        if !matches!(url.scheme(), "http" | "https" | "ssh" | "git")
            || url.port() == Some(0)
            || url.path() == "/"
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(bad_url());
        }
        let host = match Host::parse(url.host_str().ok_or_else(bad_url)?).map_err(|_| bad_url())? {
            Host::Domain(name) => normalize_host(&name).map_err(|_| bad_url())?,
            address => address.to_string(),
        };
        url.set_host(Some(&host)).map_err(|_| bad_url())?;
        if matches!(
            (url.scheme(), url.port()),
            ("ssh", Some(22)) | ("git", Some(9418))
        ) {
            url.set_port(None).map_err(|_| bad_url())?;
        }
        if authority.contains('@') {
            let user = url.username();
            if url.scheme() != "ssh"
                || user.is_empty()
                || user.len() > 128
                || user.starts_with('-')
                || !user
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(bad_url());
            }
        }
        if url
            .path()
            .as_bytes()
            .windows(3)
            .any(|bytes| bytes[0] == b'%' && encoded_control(bytes[1], bytes[2]))
        {
            return Err(bad_url());
        }
        let normalized: String = url.into();
        if normalized.len() > MAX_LOCATION_BYTES {
            return Err(bad_url());
        }
        Ok(Self(normalized))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

fn encoded_control(high: u8, low: u8) -> bool {
    (matches!(high, b'0' | b'1') && low.is_ascii_hexdigit())
        || (high == b'7' && low.eq_ignore_ascii_case(&b'f'))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Branch(String);

impl Branch {
    pub(crate) fn parse(reference: &str) -> io::Result<Self> {
        let branch = reference
            .strip_prefix("refs/heads/")
            .ok_or_else(|| invalid("The Git source did not name a branch."))?;
        if reference.len() > MAX_BRANCH_BYTES
            || branch.ends_with('.')
            || branch.contains("..")
            || branch.contains("@{")
            || branch.contains(['~', '^', ':', '?', '*', '[', '\\'])
            || branch.chars().any(char::is_whitespace)
            || unsafe_text(branch)
            || branch
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
        {
            return Err(invalid("The Git source named an invalid branch."));
        }
        Ok(Self(reference.into()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) fn read(
    url: &GitUrl,
    branch: Option<&Branch>,
    cancel: &AtomicBool,
) -> io::Result<(Branch, Vec<TeamworkEntry>)> {
    let git = Git::new(cancel)?;
    let probe = git.init("sha1")?;
    let reference = branch.map_or("HEAD", Branch::as_str);
    let advertised = git.run(
        &probe,
        &[
            "ls-remote",
            "--symref",
            "--exit-code",
            "--",
            url.as_str(),
            reference,
        ],
        MAX_REFS_OUTPUT,
    )?;
    let (branch, hash_length) = advertised_branch(&advertised, branch)?;
    let repo = if hash_length == 64 {
        git.init("sha256")?
    } else {
        probe
    };
    let refspec = format!("+{}:{TRACKING}", branch.as_str());
    git.run(
        &repo,
        &[
            "fetch",
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "--",
            url.as_str(),
            &refspec,
        ],
        0,
    )?;
    let tree = git.run(
        &repo,
        &[
            "ls-tree",
            "-lz",
            "--full-tree",
            TRACKING,
            "--",
            PRIMARY,
            LEGACY,
        ],
        MAX_REFS_OUTPUT,
    )?;
    let Some(blob) = connection_blob(&tree, hash_length)? else {
        return Ok((branch, Vec::new()));
    };
    let content = Zeroizing::new(git.run(
        &repo,
        &["cat-file", "blob", blob.hash],
        MAX_TEAMWORK_BYTES as usize,
    )?);
    if content.len() as u64 != blob.size {
        return Err(invalid("Git returned a different Teamwork file size."));
    }
    Ok((branch, read_teamwork_bytes(&content)?))
}

fn advertised_branch(bytes: &[u8], known: Option<&Branch>) -> io::Result<(Branch, usize)> {
    let output = str::from_utf8(bytes).map_err(|_| invalid("Invalid Git ref advertisement."))?;
    let requested = known.map_or("HEAD", Branch::as_str);
    let mut symbolic = None;
    let mut hash_length = None;
    for (value, _) in output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, name)| *name == requested)
    {
        if let Some(reference) = value.strip_prefix("ref: ") {
            if symbolic.is_some() || known.is_some() {
                return Err(invalid("Ambiguous Git branch advertisement."));
            }
            symbolic = Some(Branch::parse(reference)?);
        } else if hash_length.is_some() || !valid_hash(value, value.len()) {
            return Err(invalid("Invalid Git commit ID."));
        } else {
            hash_length = Some(value.len());
        }
    }
    let branch = known
        .cloned()
        .or(symbolic)
        .ok_or_else(|| invalid("The Git source has no default branch."))?;
    let hash_length = hash_length.ok_or_else(|| invalid("The Git branch has no commit."))?;
    Ok((branch, hash_length))
}

fn valid_hash(hash: &str, expected_length: usize) -> bool {
    matches!(expected_length, 40 | 64)
        && hash.len() == expected_length
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct Blob<'a> {
    hash: &'a str,
    size: u64,
}

fn connection_blob(tree: &[u8], hash_length: usize) -> io::Result<Option<Blob<'_>>> {
    let invalid_tree = || invalid("Invalid Git Teamwork tree response.");
    let mut primary = None;
    let mut legacy = None;
    let output = str::from_utf8(tree).map_err(|_| invalid_tree())?;
    if !output.is_empty() && !output.ends_with('\0') {
        return Err(invalid_tree());
    }
    for entry in output.split_terminator('\0') {
        let (metadata, path) = entry.split_once('\t').ok_or_else(invalid_tree)?;
        let selected = match path {
            PRIMARY => &mut primary,
            LEGACY => &mut legacy,
            _ => return Err(invalid_tree()),
        };
        let fields = metadata.split_ascii_whitespace().collect::<Vec<_>>();
        let [mode, kind, hash, size] = fields.as_slice() else {
            return Err(invalid_tree());
        };
        if selected.is_some() || !valid_hash(hash, hash_length) {
            return Err(invalid_tree());
        }
        *selected = Some(match (*mode, *kind) {
            ("100644" | "100755", "blob") => size
                .parse::<u64>()
                .map(|size| Blob { hash, size })
                .map_err(|_| invalid_tree()),
            _ => Err(invalid("The Git Teamwork file is not a regular blob.")),
        });
    }
    let blob = primary.or(legacy).transpose()?;
    if blob
        .as_ref()
        .is_some_and(|blob| blob.size > MAX_TEAMWORK_BYTES)
    {
        return Err(invalid("The Git Teamwork file exceeds its size limit."));
    }
    Ok(blob)
}

#[cfg(test)]
mod tests;
