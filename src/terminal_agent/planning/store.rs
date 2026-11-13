//! Private storage for explicitly saved advisory reports.

use std::fs::TryLockError;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, nonce, require_single_link};
use crate::snippet_analysis::model::{content_hash, now_millis};

use super::{Report, text};

const DIRECTORY: &str = "terminal-agent-plans";
const FILE: &str = "plans.json";
const MAX_RECORDS: usize = 32;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Destination {
    label: String,
    fingerprint: String,
}

impl Destination {
    /// The canonical identity must contain only the reviewed route identity,
    /// including any jump route and the authenticated peer key when available.
    pub(crate) fn capture(label: String, canonical_identity: &str) -> io::Result<Self> {
        if !text(&label, 512) || canonical_identity.is_empty() || canonical_identity.len() > 8192 {
            return Err(invalid("Invalid planning destination"));
        }
        Ok(Self {
            label,
            fingerprint: content_hash(canonical_identity)?,
        })
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn same_route(&self, current: &Self) -> bool {
        self.fingerprint == current.fingerprint
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) saved_at: u64,
    pub(crate) destination: Destination,
    pub(crate) goal: String,
    pub(crate) report: Report,
}

impl Record {
    pub(crate) fn new(destination: Destination, goal: String, report: Report) -> io::Result<Self> {
        let record = Self {
            id: nonce()?,
            saved_at: now_millis(),
            destination,
            goal,
            report,
        };
        if !record.valid() {
            return Err(invalid("The reviewed plan exceeds the storage limits"));
        }
        Ok(record)
    }

    fn valid(&self) -> bool {
        hex(&self.id, 32)
            && self.saved_at != 0
            && text(&self.destination.label, 512)
            && hex(&self.destination.fingerprint, 64)
            && text(&self.goal, 8192)
            && self.report.valid()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    plans: Vec<Record>,
}

pub(crate) fn list(root: &Path) -> io::Result<Vec<Record>> {
    match Directory::open(&root.join(DIRECTORY), false) {
        Ok(directory) => Ok(read(&directory)?.0.plans),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn save(root: &Path, record: Record, private: bool) -> io::Result<()> {
    if private {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Private plans remain in memory",
        ));
    }
    if !record.valid() {
        return Err(invalid("Invalid reviewed plan"));
    }
    update(root, |plans| {
        if plans.len() >= MAX_RECORDS {
            return Err(invalid("Delete an existing plan before saving another"));
        }
        if plans.iter().any(|existing| existing.id == record.id) {
            return Err(invalid("This reviewed plan is already saved"));
        }
        plans.insert(0, record);
        Ok(())
    })
}

pub(crate) fn delete(root: &Path, expected: &Record, private: bool) -> io::Result<()> {
    if private {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Private runs do not change saved plans",
        ));
    }
    update(root, |plans| {
        let index = plans
            .iter()
            .position(|record| record == expected)
            .ok_or_else(|| invalid("The selected saved plan changed or was removed"))?;
        plans.remove(index);
        Ok(())
    })
}

fn update(root: &Path, change: impl FnOnce(&mut Vec<Record>) -> io::Result<()>) -> io::Result<()> {
    let directory = Directory::open(&root.join(DIRECTORY), true)?;
    let lock = directory.open_lock(".plans.lock")?;
    require_single_link(&lock)?;
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another window is changing saved plans",
        ),
        TryLockError::Error(error) => error,
    })?;
    let result = (|| {
        let (mut document, stamp) = read(&directory)?;
        change(&mut document.plans)?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(&document).map_err(|_| invalid("Could not encode saved plans"))?,
        );
        if bytes.len() > MAX_BYTES {
            return Err(invalid("The saved plan library exceeds 1 MiB"));
        }
        match stamp {
            Some(stamp) => directory.replace_with(
                FILE,
                |file| file.write_all(&bytes),
                || stamp.require_unchanged(&directory.open_private_file(FILE)?),
            ),
            None => directory.publish_copy(FILE, &bytes),
        }
    })();
    let _ = lock.unlock();
    result
}

fn read(directory: &Directory) -> io::Result<(Document, Option<FileStamp>)> {
    let file = match directory.open_private_file(FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((
                Document {
                    version: 1,
                    plans: Vec::new(),
                },
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("The saved plan library exceeds 1 MiB"));
    }
    stamp.require_unchanged(&file)?;
    let document: Document =
        serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid saved plan library"))?;
    let mut ids = std::collections::HashSet::new();
    if document.version != 1
        || document.plans.len() > MAX_RECORDS
        || document
            .plans
            .iter()
            .any(|record| !record.valid() || !ids.insert(&record.id))
    {
        return Err(invalid("Invalid saved plan records"));
    }
    Ok((document, Some(stamp)))
}

fn hex(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    #[test]
    fn agent_plan_store_keeps_private_runs_ephemeral_and_compares_exact_destination() {
        let profile = Profile::new();
        let destination = Destination::capture("host".into(), "ssh:user@host:22/key-a").unwrap();
        let report = Report {
            title: "Change config".into(),
            summary: "Review and verify".into(),
            prerequisites: vec![],
            steps: vec!["Inspect the selected file".into()],
            risks: vec![],
            success_criteria: vec!["Service starts".into()],
        };
        let record = Record::new(
            destination.clone(),
            "Update the configuration".into(),
            report,
        )
        .unwrap();
        assert!(save(&profile.0, record.clone(), true).is_err());
        assert!(!profile.0.join(DIRECTORY).exists());
        save(&profile.0, record.clone(), false).unwrap();
        let mut stored = list(&profile.0).unwrap();
        assert_eq!(stored.len(), 1);
        let saved = stored.pop().unwrap();
        assert!(saved == record);
        assert!(saved.destination.same_route(&destination));
        let replaced = Destination::capture("host".into(), "ssh:user@host:22/key-b").unwrap();
        assert!(!saved.destination.same_route(&replaced));
        assert!(delete(&profile.0, &saved, true).is_err());
        delete(&profile.0, &saved, false).unwrap();
        assert!(list(&profile.0).unwrap().is_empty());
    }
}
