//! Root-owned policy and held path anchors for one exact sudo login.

use std::ffi::OsStr;

use kortty_server_archive::privileged_rsync::valid_path;
use serde::Deserialize;

use super::super::privileged_archive::{
    ConfigFile, Root, open_protected_directory, require_installation, select_root, within,
};
use super::super::{
    DIRECTORY, Directory, File, Mode, Sha256, absolute_file, getegid, getgid, invalid, io, open,
    same_node,
};
use sha2::Digest as _;

const CONFIG: &str = "/etc/kortty/privileged-rsync.json";
const MAX_ACCOUNTS: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    version: u16,
    accounts: Vec<Account>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Account {
    uid: u32,
    gid: u32,
    #[serde(default)]
    handoff_root: Option<String>,
    #[serde(default)]
    source_roots: Vec<String>,
    #[serde(default)]
    destination_roots: Vec<String>,
    #[serde(default)]
    replace_roots: Vec<String>,
}

impl Account {
    fn roots(&self) -> impl Iterator<Item = &String> {
        self.source_roots
            .iter()
            .chain(&self.destination_roots)
            .chain(&self.replace_roots)
    }
}

pub(super) struct Rules {
    configuration: ConfigFile,
    login: (u32, u32),
    handoff: Option<Root>,
    sources: Vec<Root>,
    destinations: Vec<Root>,
    replacements: Vec<Root>,
    revision: [u8; 32],
}

impl Rules {
    pub(super) fn load() -> io::Result<Self> {
        Self::load_for(true)
    }

    pub(super) fn load_inline_upload() -> io::Result<Self> {
        Self::load_for(false)
    }

    fn load_for(with_handoff: bool) -> io::Result<Self> {
        require_installation()?;
        if getgid().as_raw() != 0 || getegid().as_raw() != 0 {
            return Err(invalid("Privileged rsync requires the root UID and GID"));
        }
        let login = (
            sudo_id(std::env::var_os("SUDO_UID").as_deref())?,
            sudo_id(std::env::var_os("SUDO_GID").as_deref())?,
        );
        let (configuration, bytes) = ConfigFile::read(CONFIG)?;
        let account = account(&bytes, login)?;
        let sources = if with_handoff {
            open_optional(account.source_roots)?
        } else {
            Vec::new()
        };
        let destinations = open_optional(account.destination_roots)?;
        let replacements = open_optional(account.replace_roots)?;
        for root in &replacements {
            select_root(&destinations, &root.path)?.open_descendant(&root.path)?;
        }
        let handoff = with_handoff
            .then(|| {
                let path = account
                    .handoff_root
                    .ok_or_else(|| invalid("Configure an rsync handoff root for this login"))?;
                require_searchable(&path)?;
                Ok::<_, io::Error>(Root::open_all(vec![path])?.pop().expect("one handoff root"))
            })
            .transpose()?;
        let mut rules = Self {
            configuration,
            login,
            handoff,
            sources,
            destinations,
            replacements,
            revision: [0; 32],
        };
        rules.revision = rules.current_revision(&rules.configuration)?;
        Ok(rules)
    }

    pub(super) fn revision(&self) -> [u8; 32] {
        self.revision
    }

    pub(super) fn login(&self) -> (u32, u32) {
        self.login
    }

    pub(super) fn handoff(&self) -> io::Result<(String, Directory, u64)> {
        let root = self
            .handoff
            .as_ref()
            .ok_or_else(|| invalid("Configure an rsync handoff root for this login"))?;
        require_searchable(&root.path)?;
        Ok((root.path.clone(), root.open_current()?, root.mount))
    }

    pub(super) fn source_parent(&self, path: &str, parent: &str) -> io::Result<(Directory, u64)> {
        let root = select_root(&self.sources, path).map_err(|_| outside_scope())?;
        let directory = if root.path == path {
            let (_, name) = absolute_file(path)?;
            let anchor = root.open_current()?;
            let directory = open_protected_directory(parent)?;
            directory.require_child(name, &anchor.stat)?;
            directory
        } else {
            root.open_descendant(parent)?
        };
        Ok((directory, root.mount))
    }

    pub(super) fn destination_parent(&self, parent: &str, replace: bool) -> io::Result<Directory> {
        let destination = select_root(&self.destinations, parent)
            .map_err(|_| outside_scope())?
            .open_descendant(parent)?;
        if replace {
            let replacement = select_root(&self.replacements, parent)
                .map_err(|_| outside_scope())?
                .open_descendant(parent)?;
            if !same_node(&replacement.stat, &destination.stat) {
                return Err(outside_scope());
            }
        }
        Ok(destination)
    }

    pub(super) fn require_current(&self) -> io::Result<()> {
        let (configuration, _) = ConfigFile::read(CONFIG)?;
        if let Some(handoff) = &self.handoff {
            require_searchable(&handoff.path)?;
        }
        if self.current_revision(&configuration)? != self.revision {
            return Err(invalid(
                "The privileged rsync configuration or an allowed root changed",
            ));
        }
        Ok(())
    }

    fn current_revision(&self, configuration: &ConfigFile) -> io::Result<[u8; 32]> {
        let mut hash = Sha256::new_with_prefix(b"kortty-privileged-rsync-rules-v1\0");
        configuration.hash_into(&mut hash);
        hash.update(self.login.0.to_be_bytes());
        hash.update(self.login.1.to_be_bytes());
        for root in self
            .handoff
            .iter()
            .chain(&self.sources)
            .chain(&self.destinations)
            .chain(&self.replacements)
        {
            let directory = root.open_current()?;
            hash.update(directory.stat.st_dev.to_be_bytes());
            hash.update(directory.stat.st_ino.to_be_bytes());
            hash.update(directory.stat.st_mode.to_be_bytes());
            hash.update(directory.stat.st_gid.to_be_bytes());
            hash.update(root.mount.to_be_bytes());
        }
        Ok(hash.finalize().into())
    }
}

pub(super) fn require_source_file(file: &File, mount: u64) -> io::Result<()> {
    let node = Directory::from_fd(file.try_clone()?.into())?;
    super::super::privileged_archive::require_source_node(&node, false, mount)
}

fn account(bytes: &[u8], login: (u32, u32)) -> io::Result<Account> {
    let configuration: Configuration = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid privileged rsync configuration"))?;
    if configuration.version != 1 || !(1..=MAX_ACCOUNTS).contains(&configuration.accounts.len()) {
        return Err(invalid(
            "Invalid privileged rsync accounts or configuration version",
        ));
    }
    for (index, current) in configuration.accounts.iter().enumerate() {
        valid_id(current.uid)?;
        valid_id(current.gid)?;
        for root in current.roots() {
            absolute_file(root)?;
        }
        let earlier = &configuration.accounts[..index];
        if earlier
            .iter()
            .any(|other| (current.uid, current.gid) == (other.uid, other.gid))
        {
            return Err(invalid("Privileged file rules require unique accounts"));
        }
        let Some(handoff) = &current.handoff_root else {
            continue;
        };
        if handoff.len() > 960 || !valid_path(handoff) {
            return Err(invalid(
                "Choose a canonical privileged rsync handoff path of at most 960 bytes",
            ));
        }
        if earlier
            .iter()
            .filter_map(|other| other.handoff_root.as_deref())
            .any(|other| overlaps(handoff, other))
            || configuration
                .accounts
                .iter()
                .flat_map(Account::roots)
                .any(|root| overlaps(handoff, root))
        {
            return Err(invalid(
                "Privileged rsync needs unique accounts and isolated handoff roots",
            ));
        }
    }
    configuration
        .accounts
        .into_iter()
        .find(|rule| (rule.uid, rule.gid) == login)
        .ok_or_else(|| invalid("The sudo rsync UID and GID are not authorized together"))
}

fn overlaps(first: &str, second: &str) -> bool {
    within(first, second) || within(second, first)
}

fn open_optional(paths: Vec<String>) -> io::Result<Vec<Root>> {
    if paths.is_empty() {
        Ok(Vec::new())
    } else {
        Root::open_all(paths)
    }
}

fn require_searchable(path: &str) -> io::Result<()> {
    let mut parent = Directory::from_fd(open("/", DIRECTORY, Mode::empty())?)?;
    for name in path[1..].split('/') {
        parent = parent.child(name)?;
        if parent.stat.st_mode & 0o001 == 0 {
            return Err(invalid(
                "The dedicated handoff root needs search access for the SSH login",
            ));
        }
    }
    Ok(())
}

fn sudo_id(value: Option<&OsStr>) -> io::Result<u32> {
    let value = value.and_then(OsStr::to_str).ok_or_else(bad_id)?;
    let id: u32 = value.parse().map_err(|_| bad_id())?;
    if id.to_string() != value {
        return Err(bad_id());
    }
    valid_id(id)
}

fn valid_id(id: u32) -> io::Result<u32> {
    if id == 0 || id == u32::MAX {
        Err(bad_id())
    } else {
        Ok(id)
    }
}

fn bad_id() -> io::Error {
    invalid("Privileged rsync requires exact ordinary sudo UID and GID values")
}

fn outside_scope() -> io::Error {
    invalid("The rsync path is outside the configured protected roots for this action")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_rsync_roles_bind_exact_login_and_isolated_handoff() {
        let bytes = br#"{"version":1,"accounts":[
            {"uid":1000,"gid":1001,"handoff_root":"/srv/stage/alice","source_roots":["/srv/in"]},
            {"uid":1002,"gid":1003,"handoff_root":"/srv/stage/bob","destination_roots":["/srv/out"]}
        ]}"#;
        assert_eq!(
            account(bytes, (1000, 1001))
                .unwrap()
                .handoff_root
                .as_deref(),
            Some("/srv/stage/alice")
        );
        assert!(account(bytes, (1000, 1003)).is_err());
        let changed = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("/srv/out", "/srv/stage");
        assert!(account(changed.as_bytes(), (1000, 1001)).is_err());
        assert_eq!(sudo_id(Some(OsStr::new("1000"))).unwrap(), 1000);
        for invalid in ["0", "01000", "+1000", "-1", "4294967295", "1000 "] {
            assert!(sudo_id(Some(OsStr::new(invalid))).is_err());
        }
    }
}
