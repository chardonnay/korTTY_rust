//! Resolves the native Session Journal IDs captured by one Scheduler run.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io;
use std::path::Path;

use jiff::Timestamp;
use kortty_jobs::{JobId, valid_session_journal_id, valid_session_journal_ids};

use crate::private_files::Directory;
use crate::session_journal::tree::{RunStatus, SourceKind, Usage};
use crate::session_journal::{
    MAX_CATALOG_BYTES, MAX_CATALOG_NAMES, MAX_DOCUMENT_BYTES, child, directory_names, field,
    invalid, summary, viewer::Source,
};

use super::{DECISION, OPTIONS, Options, TARGET, retention::duplicate_receipts, valid_label};

pub(crate) const MAX_BULK_OPEN: usize = 8;

#[derive(Debug)]
pub(crate) struct RunJournals {
    pub(crate) rows: Vec<RunJournalRow>,
    pub(crate) limited: bool,
}

impl RunJournals {
    pub(crate) fn entries(&self) -> impl Iterator<Item = &LinkedJournal> {
        self.rows.iter().filter_map(|row| row.journal.as_ref())
    }

    pub(crate) fn missing(&self) -> usize {
        self.rows.iter().filter(|row| row.journal.is_none()).count()
    }
}

#[derive(Debug)]
pub(crate) struct RunJournalRow {
    pub(crate) id: String,
    pub(crate) journal: Option<LinkedJournal>,
}

#[derive(Clone, Debug)]
pub(crate) struct LinkedJournal {
    pub(crate) source: Source,
    pub(crate) title: String,
    pub(crate) duplicate: bool,
    pub(crate) started_at: Option<Timestamp>,
    pub(crate) status: Option<RunStatus>,
    pub(crate) usage: Usage,
    pub(crate) pinned: bool,
    pub(crate) expires_at: Option<Timestamp>,
}

struct Candidate {
    journal: LinkedJournal,
    receipts: Vec<String>,
}

pub(crate) fn resolve(root: &Path, job: &JobId, journal_ids: &[String]) -> io::Result<RunJournals> {
    if !valid_session_journal_ids(journal_ids) {
        return Err(invalid("Invalid Scheduler Session Journal references"));
    }
    let directory = match Directory::open(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(finish(journal_ids, vec![None; journal_ids.len()], false));
        }
        Err(error) => return Err(error),
    };
    let mut entries = vec![None; journal_ids.len()];
    let mut absent = Vec::new();
    for (index, id) in journal_ids.iter().enumerate() {
        let name = format!("journal-{id}");
        match directory.has_entry(&name) {
            Ok(false) => absent.push(index),
            Ok(true) => {
                let mut budget = MAX_DOCUMENT_BYTES;
                entries[index] = read_candidate(&directory, root, &name, job, &mut budget, false)
                    .ok()
                    .flatten()
                    .map(|candidate| candidate.journal);
            }
            Err(_) => {}
        }
    }
    let mut limited = false;
    if !absent.is_empty() {
        let (mut aliases, scan_limited) =
            find_aliases(&directory, root, job, journal_ids, &absent)?;
        limited = scan_limited;
        if !limited {
            for index in absent {
                let id = journal_ids[index].as_str();
                if let Some(Some(mut journal)) = aliases.remove(id)
                    && directory
                        .has_entry(&format!("journal-{id}"))
                        .is_ok_and(|present| !present)
                {
                    journal.duplicate = true;
                    entries[index] = Some(journal);
                }
            }
        }
    }
    if !directory.same_as(&Directory::open(root, false)?)? {
        return Err(invalid(
            "The session journal directory changed while reading",
        ));
    }
    Ok(finish(journal_ids, entries, limited))
}

fn finish(
    journal_ids: &[String],
    entries: Vec<Option<LinkedJournal>>,
    limited: bool,
) -> RunJournals {
    RunJournals {
        rows: journal_ids
            .iter()
            .cloned()
            .zip(entries)
            .map(|(id, journal)| RunJournalRow { id, journal })
            .collect(),
        limited,
    }
}

type Aliases<'a> = HashMap<&'a str, Option<LinkedJournal>>;

fn find_aliases<'a>(
    directory: &Directory,
    root: &Path,
    job: &JobId,
    journal_ids: &'a [String],
    absent: &[usize],
) -> io::Result<(Aliases<'a>, bool)> {
    let names = directory_names(root, MAX_CATALOG_NAMES)?;
    // A truncated scan cannot prove an exact receipt has only one owner.
    if names.len() > MAX_CATALOG_NAMES {
        return Ok((HashMap::new(), true));
    }
    let mut budget = MAX_CATALOG_BYTES;
    let mut aliases = HashMap::new();
    for name in names.iter().filter_map(|name| name.to_str()) {
        if !name
            .strip_prefix("journal-")
            .is_some_and(valid_session_journal_id)
        {
            continue;
        }
        if budget == 0 {
            return Ok((HashMap::new(), true));
        }
        let candidate = match read_candidate(directory, root, name, job, &mut budget, true) {
            Ok(Some(candidate)) => candidate,
            Err(_) if budget == 0 => return Ok((HashMap::new(), true)),
            _ => continue,
        };
        for &index in absent {
            let id = &journal_ids[index];
            if candidate.receipts.contains(id) {
                match aliases.entry(id.as_str()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Some(candidate.journal.clone()));
                    }
                    Entry::Occupied(mut entry) => {
                        entry.insert(None);
                    }
                }
            }
        }
    }
    Ok((aliases, false))
}

fn read_candidate(
    root_directory: &Directory,
    root: &Path,
    name: &str,
    job: &JobId,
    budget: &mut u64,
    alias: bool,
) -> io::Result<Option<Candidate>> {
    let directory = root_directory.subdirectory(name, false)?;
    let path = root.join(name);
    let id = name
        .strip_prefix("journal-")
        .filter(|id| valid_session_journal_id(id))
        .ok_or_else(|| invalid("Invalid native Session Journal directory"))?;
    let source = Source::from_open_directory(&path, id, &directory)?;
    source.with_document_budget(budget, |_, document| {
        let meta = child(document.root_element(), "meta")
            .ok_or_else(|| invalid("Missing session journal metadata"))?;
        if SourceKind::parse(field(meta, "sourceKind")?.as_deref().unwrap_or_default())
            != SourceKind::Job
            || field(meta, "sourceId")?.as_deref() != Some(job.as_str())
            || !field(meta, "runId")?
                .as_deref()
                .is_some_and(valid_session_journal_id)
            || !field(meta, "automationAction")?
                .as_deref()
                .is_some_and(valid_label)
            || !field(meta, TARGET)?.as_deref().is_some_and(valid_label)
        {
            return Ok(None);
        }
        let Some(options) = field(meta, OPTIONS)? else {
            return Ok(None);
        };
        let options: Options = serde_json::from_str(&options)?;
        options.validate()?;
        if !options.enabled {
            return Ok(None);
        }
        let receipts = if alias {
            if field(meta, DECISION)?.as_deref() != Some("keep") {
                return Ok(None);
            }
            let receipts = duplicate_receipts(meta)?;
            if receipts.is_empty() {
                return Ok(None);
            }
            receipts
        } else {
            Vec::new()
        };
        let row = summary(&path, document)?;
        let title = if row.connection_name.is_empty() {
            row.title
        } else {
            format!("{} · {}", row.title, row.connection_name)
        };
        Ok(Some(Candidate {
            journal: LinkedJournal {
                source: source.clone(),
                title,
                duplicate: false,
                started_at: row.started_at,
                status: row.run_status,
                usage: row.usage,
                pinned: row.pinned,
                expires_at: row.expires_at,
            },
            receipts,
        }))
    })
}
