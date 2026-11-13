//! Bounded manifest and two-phase publication protocol for a private rsync tree.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub const ARGUMENT: &str = "--rsync-tree-v1";
pub const COMMAND: &str = "exec /usr/local/bin/kortty-server-archive --rsync-tree-v1";
pub const MAX_ENTRIES: u32 = 4096;
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_PATH_BYTES: usize = 2 * 1024 * 1024;
const FRAME_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub entries: u32,
    pub files: u32,
    pub bytes: u64,
    pub digest: [u8; 32],
}

impl Inventory {
    pub fn valid(self) -> bool {
        self.entries <= MAX_ENTRIES && self.files <= self.entries && self.bytes <= MAX_BYTES
    }
}

pub struct Manifest {
    entries: u32,
    files: u32,
    bytes: u64,
    paths: usize,
    hash: Sha256,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            entries: 0,
            files: 0,
            bytes: 0,
            paths: 0,
            hash: Sha256::new_with_prefix(b"kortty-rsync-tree-v1\0"),
        }
    }
}

impl Manifest {
    /// Visit root-relative entries depth first, sorting each level by UTF-8 bytes.
    pub fn add(&mut self, path: &str, file: Option<(u64, [u8; 32])>) -> io::Result<()> {
        self.entries += 1;
        self.paths += path.len();
        if self.entries > MAX_ENTRIES || self.paths > MAX_PATH_BYTES || path.len() > 4096 {
            return Err(invalid("Rsync directory exceeds its entry or path limits"));
        }
        self.hash.update((path.len() as u32).to_be_bytes());
        self.hash.update(path.as_bytes());
        if let Some((size, digest)) = file {
            self.files += 1;
            self.bytes = self.bytes.saturating_add(size);
            if self.bytes > MAX_BYTES {
                return Err(invalid("Rsync directory exceeds its byte limit"));
            }
            self.hash.update([1]);
            self.hash.update(size.to_be_bytes());
            self.hash.update(digest);
        } else {
            self.hash.update([0]);
        }
        Ok(())
    }

    pub fn remaining_entries(&self) -> usize {
        MAX_ENTRIES.saturating_sub(self.entries) as usize
    }

    pub fn remaining_bytes(&self) -> u64 {
        MAX_BYTES.saturating_sub(self.bytes)
    }

    pub fn finish(self) -> Inventory {
        Inventory {
            entries: self.entries,
            files: self.files,
            bytes: self.bytes,
            digest: self.hash.finalize().into(),
        }
    }
}

pub fn hash(mut input: impl Read, size: u64) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buffer = [0; 32 * 1024];
    while remaining != 0 {
        let count = buffer.len().min(remaining as usize);
        input.read_exact(&mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(invalid("Rsync directory file size changed"));
    }
    Ok(hasher.finalize().into())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', ':'])
        && !name.ends_with(['.', ' '])
        && !name.chars().any(char::is_control)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    pub destination: String,
    pub expected: Inventory,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Verify,
    Commit,
    Abort,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Prepared {
        stage: String,
    },
    Verified,
    Published {
        destination: String,
        inventory: Inventory,
    },
    Aborted,
    Failed {
        message: String,
        outcome_unknown: bool,
    },
}

pub fn valid_stage(name: &str) -> bool {
    name.strip_prefix(".kortty-")
        .and_then(|name| name.strip_suffix(".kortty-part"))
        .is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

pub fn read_frame<T: DeserializeOwned>(mut input: impl Read) -> io::Result<T> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if !(1..=FRAME_BYTES).contains(&size) {
        return Err(invalid("Invalid rsync tree frame size"));
    }
    let mut data = vec![0; size];
    input.read_exact(&mut data)?;
    serde_json::from_slice(&data).map_err(|_| invalid("Invalid rsync tree message"))
}

pub fn write_frame(mut output: impl Write, message: &impl Serialize) -> io::Result<()> {
    let data = serde_json::to_vec(message).map_err(io::Error::other)?;
    if data.len() > FRAME_BYTES {
        return Err(invalid("Invalid rsync tree frame size"));
    }
    output.write_all(&(data.len() as u32).to_be_bytes())?;
    output.write_all(&data)?;
    output.flush()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
