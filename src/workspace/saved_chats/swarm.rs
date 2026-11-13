//! Owner-only Swarm drafts, completed conversations, and imported archives.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::Path;

use kortty_jobs::{
    MAX_SWARM_QUESTION_BYTES, MAX_SWARM_TARGET_DETAIL_BYTES, MAX_SWARM_TARGET_LABEL_BYTES,
    MAX_SWARM_TARGETS, ProfileRef, RunOutcome, SwarmReport, SwarmTargetReport,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::session_journal::is_display_character;
use crate::terminal_agent::swarm::MAX_TARGETS;
use crate::terminal_agent::swarm::comparison::MAX_MARKDOWN_BYTES;

use super::store::{fingerprint, lock, valid_id};

pub(super) mod java_archive;

use java_archive::Archive;

pub(super) const FOLDER: &str = "swarm-conversations";
const VERSION: u8 = 1;
pub(super) const MAX_CHATS: usize = 128;
// Deleted origins stay reserved. At capacity, refuse new origins without pruning.
pub(super) const MAX_ORIGINS: usize = 4096;
pub(super) const MAX_ENTRIES: usize = MAX_ORIGINS + 16;
const MAX_TURNS: usize = 16;
pub(super) const MAX_BYTES: usize = 768 * 1024;
const MAX_PROFILE_NAME_BYTES: usize = 512;
const MAX_TITLE_CHARS: usize = 120;

/// An opaque reservation for exactly one scheduler seed, independent of numeric run IDs.
#[derive(Clone, Eq, PartialEq)]
pub(in crate::workspace) struct Origin(String);

impl Origin {
    #[cfg(test)]
    fn new() -> io::Result<Self> {
        crate::private_files::nonce().map(Self)
    }

    pub(in crate::workspace) fn from_id(id: &str) -> io::Result<Self> {
        valid_id(id)
            .then(|| Self(id.to_owned()))
            .ok_or_else(|| invalid("Invalid saved Swarm conversation identifier"))
    }

    pub(in crate::workspace) fn id(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Turn {
    pub(in crate::workspace) question: String,
    pub(in crate::workspace) targets: Vec<SwarmTargetReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) comparison: Option<String>,
    // Legacy turns stored only saved SSH connections, using the connection ID
    // as the report ID. An explicit route list allows archived live targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) routes: Option<Vec<SavedRoute>>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct SavedRoute {
    pub(in crate::workspace) target: u64,
    pub(in crate::workspace) connection: u64,
    pub(in crate::workspace) directory: Option<String>,
    pub(in crate::workspace) group: Option<String>,
}

impl From<SwarmReport> for Turn {
    fn from(report: SwarmReport) -> Self {
        Self {
            question: report.question,
            targets: report.targets,
            comparison: report.comparison,
            routes: None,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace) struct Record {
    version: u8,
    pub(in crate::workspace) id: String,
    pub(in crate::workspace) revision: u64,
    seed_profile_id: ProfileRef,
    pub(in crate::workspace) profile_id: ProfileRef,
    pub(in crate::workspace) profile_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) draft_routes: Option<Vec<SavedRoute>>,
    pub(in crate::workspace) turns: Vec<Turn>,
    updated: i64,
    #[serde(skip)]
    pub(in crate::workspace) sync_failed: bool,
    #[serde(skip)]
    fingerprint: [u8; 32],
}

impl Record {
    fn new(id: String, profile_id: ProfileRef, profile_name: &str) -> Self {
        Self {
            version: VERSION,
            id,
            revision: 1,
            seed_profile_id: profile_id.clone(),
            profile_id,
            profile_name: profile_name.to_owned(),
            title: None,
            draft_routes: None,
            turns: Vec::new(),
            updated: now(),
            sync_failed: false,
            fingerprint: [0; 32],
        }
    }

    pub(in crate::workspace) fn summary(&self) -> Summary {
        Summary::from(self)
    }

    pub(in crate::workspace) fn same_revision(&self, other: &Self) -> bool {
        self.id == other.id
            && self.revision == other.revision
            && self.fingerprint == other.fingerprint
    }

    pub(in crate::workspace) fn title(&self) -> &str {
        if let Some(title) = &self.title {
            return title;
        }
        let line = self
            .turns
            .first()
            .and_then(|turn| {
                turn.question
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
            })
            .unwrap_or_default();
        let end = line
            .char_indices()
            .nth(MAX_TITLE_CHARS)
            .map_or(line.len(), |(index, _)| index);
        &line[..end]
    }

    pub(in crate::workspace) fn is_draft(&self) -> bool {
        self.draft_routes.is_some()
    }

    pub(in crate::workspace) fn target_ids(&self) -> impl Iterator<Item = u64> + '_ {
        let prior_turn = self.turns.last().into_iter().flat_map(|turn| {
            turn.targets.iter().filter_map(|target| match &turn.routes {
                Some(routes) => routes
                    .iter()
                    .find(|route| route.target == target.target)
                    .map(|route| route.connection),
                None => Some(target.target),
            })
        });
        self.draft_routes
            .iter()
            .flatten()
            .map(|route| route.connection)
            .chain(prior_turn)
    }

    pub(in crate::workspace) fn target_route(&self, connection: u64) -> Option<&SavedRoute> {
        let routes = match &self.draft_routes {
            Some(routes) => routes,
            None => self.turns.last()?.routes.as_ref()?,
        };
        routes.iter().find(|route| route.connection == connection)
    }
}

#[derive(Clone)]
pub(in crate::workspace) struct Summary {
    pub(in crate::workspace) id: String,
    pub(in crate::workspace) revision: u64,
    pub(in crate::workspace) profile_name: String,
    pub(in crate::workspace) title: String,
    pub(in crate::workspace) is_draft: bool,
    pub(super) is_java_archive: bool,
    pub(in crate::workspace) target_count: usize,
    pub(in crate::workspace) updated: i64,
    fingerprint: [u8; 32],
}

impl Summary {
    pub(in crate::workspace) fn matches(&self, record: &Record) -> bool {
        self.id == record.id
            && self.revision == record.revision
            && self.fingerprint == record.fingerprint
    }

    pub(super) fn matches_archive(&self, archive: &Archive) -> bool {
        self.is_java_archive && self.id == archive.id && self.fingerprint == archive.fingerprint
    }
}

impl From<&Record> for Summary {
    fn from(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            revision: record.revision,
            profile_name: record.profile_name.clone(),
            title: record.title().to_owned(),
            is_draft: record.is_draft(),
            is_java_archive: false,
            target_count: record.draft_routes.as_ref().map_or_else(
                || record.turns.last().map_or(0, |turn| turn.targets.len()),
                Vec::len,
            ),
            updated: record.updated,
            fingerprint: record.fingerprint,
        }
    }
}

#[derive(Clone, Default)]
pub(in crate::workspace) struct Listing {
    pub(in crate::workspace) chats: Vec<Summary>,
    pub(in crate::workspace) unavailable: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum Stored {
    Conversation(Record),
    JavaArchive(Box<Archive>),
    Deleted { version: u8, id: String },
}

impl Stored {
    fn id(&self) -> &str {
        match self {
            Self::Conversation(record) => &record.id,
            Self::JavaArchive(record) => &record.id,
            Self::Deleted { id, .. } => id,
        }
    }

    fn validate(&self, id: &str) -> io::Result<()> {
        if self.id() != id {
            return Err(invalid(
                "Saved Swarm conversation identity does not match its file",
            ));
        }
        match self {
            Self::Conversation(record) if valid_record(record) => Ok(()),
            Self::JavaArchive(record) if record.valid() => Ok(()),
            Self::Deleted {
                version: VERSION, ..
            } => Ok(()),
            _ => Err(invalid("Invalid saved Swarm conversation")),
        }
    }

    fn into_record(self) -> io::Result<Record> {
        match self {
            Self::Conversation(record) => Ok(record),
            Self::JavaArchive(_) => Err(invalid("This Swarm conversation is a Java archive")),
            Self::Deleted { .. } => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "The saved Swarm conversation was deleted.",
            )),
        }
    }
}

pub(in crate::workspace) fn list(root: &Path) -> io::Result<Listing> {
    match open(root, false) {
        Ok(directory) => listing(&directory).map(|(listing, _)| listing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Listing::default()),
        Err(error) => Err(error),
    }
}

pub(in crate::workspace) fn load(root: &Path, id: &str) -> io::Result<Record> {
    read(&open(root, false)?, id)?.0.into_record()
}

pub(super) fn load_archive(root: &Path, entry: &Summary) -> io::Result<Box<Archive>> {
    let directory = open(root, false)?;
    read_archive(&directory, entry).map(|(archive, _)| archive)
}

fn read_archive(directory: &Directory, entry: &Summary) -> io::Result<(Box<Archive>, FileStamp)> {
    match read(directory, &entry.id)? {
        (Stored::JavaArchive(archive), stamp) if entry.matches_archive(&archive) => {
            Ok((archive, stamp))
        }
        _ => Err(conflict()),
    }
}

pub(super) fn delete_archive(root: &Path, entry: &Summary) -> io::Result<bool> {
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (_, expected) = read_archive(&directory, entry)?;
    let deleted = Stored::Deleted {
        version: VERSION,
        id: entry.id.clone(),
    };
    let bytes = encode(&deleted)?;
    directory.replace_with(
        &filename(&entry.id)?,
        |file| file.write_all(&bytes),
        || {
            let (_, actual) = read_archive(&directory, entry)?;
            if actual == expected {
                Ok(())
            } else {
                Err(conflict())
            }
        },
    )?;
    Ok(directory.sync().is_err())
}

/// Publishes an initial committed report once. A deleted origin stays deleted.
pub(in crate::workspace) fn seed(
    root: &Path,
    origin: &Origin,
    profile_id: ProfileRef,
    profile_name: &str,
    report: SwarmReport,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Option<Record>> {
    seed_turn(root, origin, profile_id, profile_name, report.into(), check)
}

/// Publishes a new interactive conversation with a locally allocated identity.
pub(in crate::workspace) fn save_interactive(
    root: &Path,
    profile_id: ProfileRef,
    profile_name: &str,
    turn: Turn,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    require_interactive_targets(&turn)?;
    let origin = Origin(crate::private_files::nonce()?);
    seed_turn(root, &origin, profile_id, profile_name, turn, check)?.ok_or_else(conflict)
}

fn seed_turn(
    root: &Path,
    origin: &Origin,
    profile_id: ProfileRef,
    profile_name: &str,
    turn: Turn,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Option<Record>> {
    check()?;
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    match read(&directory, origin.id()) {
        Ok((Stored::Deleted { .. }, _)) => {
            check()?;
            directory.sync()?;
            return Ok(None);
        }
        Ok((Stored::Conversation(mut record), _)) => {
            if record.seed_profile_id != profile_id
                || !record.turns.first().is_some_and(|initial| {
                    initial.question == turn.question
                        && initial.targets == turn.targets
                        && initial.routes == turn.routes
                        && turn
                            .comparison
                            .as_ref()
                            .is_none_or(|expected| initial.comparison.as_ref() == Some(expected))
                })
            {
                return Err(conflict());
            }
            check()?;
            record.sync_failed = directory.sync().is_err();
            return Ok(Some(record));
        }
        Ok((Stored::JavaArchive(_), _)) => return Err(conflict()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut record = Record::new(origin.id().to_owned(), profile_id, profile_name);
    record.turns.push(turn);
    publish_initial(&directory, record, check).map(Some)
}

fn publish_initial(
    directory: &Directory,
    record: Record,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<Record> {
    let (listing, origins) = listing(directory)?;
    if origins >= MAX_ORIGINS {
        return Err(invalid("The saved Swarm origin limit has been reached"));
    }
    if listing.chats.len() + listing.unavailable >= MAX_CHATS {
        return Err(invalid("The saved Swarm conversation library is full"));
    }
    write(directory, Stored::Conversation(record), None, check)?
        .0
        .into_record()
}

/// Publishes a named interactive conversation with a completed first turn.
pub(in crate::workspace) fn save_interactive_named(
    root: &Path,
    profile_id: ProfileRef,
    profile_name: &str,
    title: String,
    turn: Turn,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    require_interactive_targets(&turn)?;
    check()?;
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    let mut record = Record::new(crate::private_files::nonce()?, profile_id, profile_name);
    record.title = Some(title);
    record.turns.push(turn);
    publish_initial(&directory, record, check)
}

/// Publishes the reviewed target hints for a new draft. Drafts contain no turns.
pub(in crate::workspace) fn save_draft(
    root: &Path,
    profile_id: ProfileRef,
    profile_name: &str,
    title: String,
    routes: Vec<SavedRoute>,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    require_draft_targets(&routes)?;
    check()?;
    let directory = open(root, true)?;
    let _lock = lock(&directory)?;
    let mut record = Record::new(crate::private_files::nonce()?, profile_id, profile_name);
    record.title = Some(title);
    record.draft_routes = Some(routes);
    publish_initial(&directory, record, check)
}

/// Replaces the exact draft the user edited, keeping its identity and empty history.
pub(in crate::workspace) fn save_named_draft_changes(
    root: &Path,
    previous: &Record,
    profile_id: ProfileRef,
    profile_name: &str,
    title: String,
    routes: Vec<SavedRoute>,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    require_draft_targets(&routes)?;
    check()?;
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (mut current, stamp) = require_current(&directory, previous)?;
    if !current.is_draft() {
        return Err(invalid("The saved Swarm conversation already has a turn"));
    }
    current.title = Some(title);
    current.draft_routes = Some(routes);
    update_profile(&mut current, profile_id, profile_name)?;
    write(
        &directory,
        Stored::Conversation(current),
        Some((previous, &stamp)),
        check,
    )?
    .0
    .into_record()
}

fn require_draft_targets(routes: &[SavedRoute]) -> io::Result<()> {
    if routes.is_empty() {
        return Err(invalid("Select a saved SSH target for the Swarm draft"));
    }
    Ok(())
}

fn require_interactive_targets(turn: &Turn) -> io::Result<()> {
    if !(1..=MAX_TARGETS).contains(&turn.targets.len()) {
        return Err(invalid(
            "Invalid target count for an interactive Swarm turn",
        ));
    }
    Ok(())
}

fn update_profile(
    record: &mut Record,
    profile_id: ProfileRef,
    profile_name: &str,
) -> io::Result<()> {
    next_revision(record)?;
    record.profile_id = profile_id;
    record.profile_name = profile_name.to_owned();
    Ok(())
}

fn next_revision(record: &mut Record) -> io::Result<()> {
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("The saved Swarm conversation has reached its revision limit"))?;
    record.updated = now().max(record.updated);
    Ok(())
}

/// Renames the exact saved conversation while retaining its identity and turns.
pub(in crate::workspace) fn rename(
    root: &Path,
    previous: &Record,
    title: String,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    if !valid_title(&title) {
        return Err(invalid("Choose a valid title for the Swarm conversation"));
    }
    check()?;
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (mut current, stamp) = require_current(&directory, previous)?;
    if current.is_draft() {
        return Err(invalid("Edit the saved Swarm draft to change its title"));
    }
    if current.title() == title {
        return Ok(current);
    }
    current.title = Some(title);
    next_revision(&mut current)?;
    write(
        &directory,
        Stored::Conversation(current),
        Some((previous, &stamp)),
        check,
    )?
    .0
    .into_record()
}

/// Appends one completed turn to the exact conversation read for its review.
pub(in crate::workspace) fn append(
    root: &Path,
    previous: &Record,
    profile_id: ProfileRef,
    profile_name: &str,
    turn: impl Into<Turn>,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    let turn = turn.into();
    require_interactive_targets(&turn)?;
    check()?;
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (mut current, stamp) = require_current(&directory, previous)?;
    if current.turns.len() >= MAX_TURNS {
        return Err(invalid(
            "This saved Swarm conversation has reached its turn limit",
        ));
    }
    update_profile(&mut current, profile_id, profile_name)?;
    current.turns.push(turn);
    current.draft_routes = None;
    write(
        &directory,
        Stored::Conversation(current),
        Some((previous, &stamp)),
        check,
    )?
    .0
    .into_record()
}

/// Attaches one reviewed comparison to the exact latest turn without appending a turn.
pub(in crate::workspace) fn set_comparison(
    root: &Path,
    previous: &Record,
    comparison: String,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<Record> {
    check()?;
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (mut current, stamp) = require_current(&directory, previous)?;
    let turn = current
        .turns
        .last_mut()
        .ok_or_else(|| invalid("Save a completed Swarm turn before adding a comparison"))?;
    if turn.comparison.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "This turn already has a saved Swarm comparison.",
        ));
    }
    turn.comparison = Some(comparison);
    current.updated = now().max(current.updated);
    write(
        &directory,
        Stored::Conversation(current),
        Some((previous, &stamp)),
        check,
    )?
    .0
    .into_record()
}

/// Atomically removes conversation content and keeps its origin reserved.
pub(in crate::workspace) fn delete(root: &Path, previous: &Record) -> io::Result<bool> {
    let directory = open(root, false)?;
    let _lock = lock(&directory)?;
    let (_, stamp) = require_current(&directory, previous)?;
    let deleted = Stored::Deleted {
        version: VERSION,
        id: previous.id.clone(),
    };
    write(&directory, deleted, Some((previous, &stamp)), || Ok(()))
        .map(|(_, sync_failed)| sync_failed)
}

fn open(root: &Path, create: bool) -> io::Result<Directory> {
    Directory::open(root, create)?.private_child(FOLDER, create)
}

fn listing(directory: &Directory) -> io::Result<(Listing, usize)> {
    let mut listing = Listing::default();
    let mut origins = 0;
    for name in directory.names(MAX_ENTRIES)? {
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        origins += 1;
        match read(directory, id) {
            Ok((Stored::Conversation(record), _)) => listing.chats.push(record.summary()),
            Ok((Stored::JavaArchive(record), _)) => listing.chats.push(record.summary()),
            Ok((Stored::Deleted { .. }, _)) => {}
            Err(_) => listing.unavailable += 1,
        }
        if origins > MAX_ORIGINS || listing.chats.len() + listing.unavailable > MAX_CHATS {
            return Err(invalid(
                "The saved Swarm conversation library exceeds its limits",
            ));
        }
    }
    listing
        .chats
        .sort_by(|a, b| (b.updated, &b.id).cmp(&(a.updated, &a.id)));
    Ok((listing, origins))
}

fn filename(id: &str) -> io::Result<String> {
    if !valid_id(id) {
        return Err(invalid("Invalid saved Swarm conversation identifier"));
    }
    Ok(format!("{id}.json"))
}

fn read(directory: &Directory, id: &str) -> io::Result<(Stored, FileStamp)> {
    let file = directory.open_private_file(&filename(id)?)?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    let mut stored = decode(&bytes, id)?;
    match &mut stored {
        Stored::Conversation(record) => record.fingerprint = fingerprint(&bytes)?,
        Stored::JavaArchive(record) => record.fingerprint = fingerprint(&bytes)?,
        Stored::Deleted { .. } => {}
    }
    Ok((stored, stamp))
}

pub(super) fn decode(bytes: &[u8], id: &str) -> io::Result<Stored> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Saved Swarm conversation exceeds the file limit"));
    }
    let stored: Stored = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid saved Swarm conversation format"))?;
    stored.validate(id)?;
    Ok(stored)
}

fn write(
    directory: &Directory,
    mut stored: Stored,
    previous: Option<(&Record, &FileStamp)>,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<(Stored, bool)> {
    let id = stored.id();
    let name = filename(id)?;
    let bytes = encode(&stored)?;
    let digest = if matches!(stored, Stored::Conversation(_)) {
        Some(fingerprint(&bytes)?)
    } else {
        None
    };
    if let Some((record, stamp)) = previous {
        directory.replace_with(
            &name,
            |file| file.write_all(&bytes),
            || {
                check()?;
                if &require_current(directory, record)?.1 != stamp {
                    return Err(conflict());
                }
                Ok(())
            },
        )?;
    } else {
        directory.publish_checked(&name, |file| file.write_all(&bytes), check)?;
    }
    let sync_failed = directory.sync().is_err();
    if let Stored::Conversation(record) = &mut stored {
        record.fingerprint = digest.expect("conversation digest was computed before publication");
        record.sync_failed = sync_failed;
    }
    Ok((stored, sync_failed))
}

fn encode(stored: &Stored) -> io::Result<Zeroizing<Vec<u8>>> {
    stored.validate(stored.id())?;
    let bytes = Zeroizing::new(
        serde_json::to_vec(stored)
            .map_err(|_| invalid("Could not encode saved Swarm conversation"))?,
    );
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Saved Swarm conversation exceeds the file limit"));
    }
    Ok(bytes)
}

fn require_current(directory: &Directory, previous: &Record) -> io::Result<(Record, FileStamp)> {
    let (stored, stamp) = read(directory, &previous.id)?;
    let current = stored.into_record()?;
    if !current.same_revision(previous) {
        return Err(conflict());
    }
    Ok((current, stamp))
}

fn valid_record(record: &Record) -> bool {
    let turn_count = record.turns.len();
    record.version == VERSION
        && record.seed_profile_id.is_nonzero()
        && record.profile_id.is_nonzero()
        && valid_line(&record.profile_name, MAX_PROFILE_NAME_BYTES)
        && turn_count <= MAX_TURNS
        && match (&record.title, &record.draft_routes) {
            (None, None) => turn_count > 0 && record.revision == turn_count as u64,
            (Some(title), None) => {
                turn_count > 0 && record.revision >= turn_count as u64 && valid_title(title)
            }
            (Some(title), Some(routes)) => {
                turn_count == 0
                    && record.revision > 0
                    && valid_title(title)
                    && valid_draft_routes(routes)
            }
            (None, Some(_)) => false,
        }
        && record.turns.iter().all(valid_turn)
}

pub(in crate::workspace) fn valid_title(title: &str) -> bool {
    valid_line(title, MAX_TITLE_CHARS * 4)
        && title.trim() == title
        && title.chars().count() <= MAX_TITLE_CHARS
}

fn valid_draft_routes(routes: &[SavedRoute]) -> bool {
    // Restoring a portable draft may revoke every saved connection hint.
    let mut connections = HashSet::new();
    routes.len() <= MAX_TARGETS
        && routes.iter().all(|route| {
            route.target == route.connection
                && connections.insert(route.connection)
                && valid_route(route)
        })
}

fn valid_turn(turn: &Turn) -> bool {
    let mut ids = HashSet::new();
    valid_text(&turn.question, MAX_SWARM_QUESTION_BYTES)
        && turn.comparison.as_deref().is_none_or(|comparison| {
            comparison.trim() == comparison && valid_text(comparison, MAX_MARKDOWN_BYTES)
        })
        && (1..=MAX_SWARM_TARGETS).contains(&turn.targets.len())
        && turn.targets.iter().all(|target| {
            target.target != 0
                && ids.insert(target.target)
                && (turn.routes.is_some() || target.outcome != RunOutcome::Cancelled)
                && valid_text(&target.label, MAX_SWARM_TARGET_LABEL_BYTES)
                && valid_text(&target.detail, MAX_SWARM_TARGET_DETAIL_BYTES)
        })
        && turn.routes.as_ref().is_none_or(|routes| {
            let mut targets = HashSet::new();
            let mut connections = HashSet::new();
            routes.len() <= ids.len()
                && routes.iter().all(|route| {
                    ids.contains(&route.target)
                        && targets.insert(route.target)
                        && connections.insert(route.connection)
                        && valid_route(route)
                })
        })
}

fn valid_route(route: &SavedRoute) -> bool {
    route.connection != 0
        && route
            .directory
            .as_deref()
            .is_none_or(|directory| directory.starts_with('/') && valid_line(directory, 4096))
        && route
            .group
            .as_deref()
            .is_none_or(|group| valid_line(group, crate::connection_store::MAX_GROUP_PATH_BYTES))
}

fn valid_line(text: &str, limit: usize) -> bool {
    valid_text(text, limit) && !text.contains(['\n', '\t'])
}

fn valid_text(text: &str, limit: usize) -> bool {
    text.len() <= limit
        && !text.trim().is_empty()
        && !text.contains(['\u{2028}', '\u{2029}'])
        && text.chars().all(is_display_character)
}

fn now() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn conflict() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "The saved Swarm conversation changed. Reopen it before saving again.",
    )
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn report(question: &str, targets: &[u64]) -> SwarmReport {
        SwarmReport {
            question: question.into(),
            targets: targets
                .iter()
                .map(|&target| SwarmTargetReport {
                    target,
                    label: format!("Target {target}"),
                    outcome: RunOutcome::Success,
                    detail: format!("Masked summary {target}"),
                })
                .collect(),
            comparison: None,
        }
    }

    fn draft_route(connection: u64, directory: &str) -> SavedRoute {
        SavedRoute {
            target: connection,
            connection,
            directory: Some(directory.into()),
            group: Some("Operations/Build".into()),
        }
    }

    #[test]
    fn single_target_scheduled_swarm_reopens_and_accepts_a_follow_up() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let initial = seed(
            &root,
            &origin,
            ProfileRef::Numeric(10),
            "Model A",
            report("Scheduled result", &[74]),
            || Ok(()),
        )?
        .expect("new scheduled Swarm conversation");
        let reopened = load(&root, &initial.id)?;
        assert_eq!(reopened.target_ids().collect::<Vec<_>>(), [74]);
        let updated = append(
            &root,
            &reopened,
            ProfileRef::Numeric(10),
            "Model A",
            report("Reviewed interactive follow-up", &[74]),
            || Ok(()),
        )?;
        let reopened = load(&root, &updated.id)?;
        assert_eq!(reopened.target_ids().collect::<Vec<_>>(), [74]);
        assert_eq!(reopened.turns.len(), 2);
        assert_eq!(reopened.turns[1].question, "Reviewed interactive follow-up");
        Ok(())
    }

    #[test]
    fn wide_scheduled_swarm_history_survives_a_bounded_interactive_follow_up() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let ids = (1..=MAX_SWARM_TARGETS as u64).rev().collect::<Vec<_>>();
        let wide = report("Wide scheduled results", &ids);
        let record = seed(
            &root,
            &origin,
            ProfileRef::Numeric(10),
            "Model A",
            wide.clone(),
            || Ok(()),
        )?
        .expect("new scheduled Swarm conversation");
        assert_eq!(record.target_ids().collect::<Vec<_>>(), ids);
        assert_eq!(record.summary().target_count, MAX_SWARM_TARGETS);
        assert!(matches!(
            append(&root, &record, ProfileRef::Numeric(10), "Model A", wide.clone(), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));
        assert!(matches!(
            save_interactive_named(&root, ProfileRef::Numeric(10), "Model A", "Wide".into(), wide.clone().into(), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));

        let interactive_ids = &ids[..MAX_TARGETS];
        let follow_up = report("Reviewed interactive follow-up", interactive_ids);
        let updated = append(
            &root,
            &record,
            ProfileRef::Numeric(11),
            "Model B",
            follow_up,
            || Ok(()),
        )?;
        let reopened = load(&root, &updated.id)?;
        assert_eq!(reopened.target_ids().collect::<Vec<_>>(), interactive_ids);
        assert_eq!(reopened.turns.len(), 2);
        assert!(reopened.turns[0] == Turn::from(wide.clone()));
        assert!(
            seed(
                &root,
                &origin,
                ProfileRef::Numeric(10),
                "Model A",
                wide,
                || Ok(())
            )?
            .is_some_and(|replayed| replayed.same_revision(&reopened))
        );
        Ok(())
    }

    #[test]
    fn named_swarm_draft_updates_exact_revision_and_keeps_title_on_first_turn() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let draft = save_draft(
            &root,
            ProfileRef::Numeric(10),
            "Model A",
            "Target review Ω".into(),
            vec![draft_route(8, "/build"), draft_route(3, "/release")],
            || Ok(()),
        )?;
        let reopened = load(&root, &draft.id)?;
        assert!(reopened.same_revision(&draft));
        assert!(reopened.is_draft());
        assert!(reopened.turns.is_empty());
        assert_eq!(reopened.revision, 1);
        assert_eq!(reopened.title(), "Target review Ω");
        assert_eq!(reopened.target_ids().collect::<Vec<_>>(), [8, 3]);
        assert_eq!(
            reopened
                .target_route(3)
                .and_then(|route| route.directory.as_deref()),
            Some("/release"),
        );
        let row = list(&root)?.chats.pop().unwrap();
        assert!(row.matches(&reopened));
        assert!(row.is_draft);
        assert_eq!(row.target_count, 2);
        assert!(matches!(
            set_comparison(&root, &reopened, "A comparison".into(), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::InvalidData
        ));

        let revise = |previous| {
            save_named_draft_changes(
                &root,
                previous,
                ProfileRef::Numeric(11),
                "Model B",
                "New title".into(),
                vec![draft_route(3, "/new")],
                || Ok(()),
            )
        };
        let edited = revise(&reopened)?;
        assert!(edited.is_draft());
        assert_eq!(edited.id, draft.id);
        assert_eq!(edited.revision, 2);
        assert_eq!(edited.profile_id, ProfileRef::Numeric(11));
        assert_eq!(edited.target_ids().collect::<Vec<_>>(), [3]);
        assert!(
            matches!(revise(&reopened), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );

        let first = report("First actual question", &[3, 12]);
        let completed = append(
            &root,
            &edited,
            ProfileRef::Numeric(11),
            "Model B",
            first.clone(),
            || Ok(()),
        )?;
        let completed = load(&root, &completed.id)?;
        assert!(!completed.is_draft());
        assert_eq!(completed.revision, 3);
        assert_eq!(completed.title(), "New title");
        assert_eq!(completed.target_ids().collect::<Vec<_>>(), [3, 12]);
        assert!(completed.target_route(3).is_none());
        assert_eq!(completed.turns.len(), 1);
        assert!(completed.turns[0] == Turn::from(first));
        assert!(
            matches!(revise(&completed), Err(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        Ok(())
    }

    #[test]
    fn named_swarm_saves_bound_title_and_validate_draft_route_hints() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        assert!(valid_title(&"界".repeat(MAX_TITLE_CHARS)));
        for title in [
            "",
            " leading",
            "two\nlines",
            "hidden\u{202e}text",
            &"界".repeat(MAX_TITLE_CHARS + 1),
        ] {
            assert!(!valid_title(title));
        }
        let save = |routes| {
            save_draft(
                &root,
                ProfileRef::Numeric(1),
                "Local model",
                "Draft title".into(),
                routes,
                || Ok(()),
            )
        };
        let mut wrong_target = draft_route(1, "/build");
        wrong_target.target = 2;
        for routes in [
            Vec::new(),
            vec![draft_route(0, "/build")],
            vec![draft_route(1, "relative")],
            vec![wrong_target],
            vec![draft_route(1, "/build"), draft_route(1, "/other")],
            (1..=MAX_TARGETS as u64 + 1)
                .map(|id| draft_route(id, "/build"))
                .collect(),
        ] {
            assert!(
                matches!(save(routes), Err(error) if error.kind() == io::ErrorKind::InvalidData)
            );
        }
        assert!(list(&root)?.chats.is_empty());

        let named = save_interactive_named(
            &root,
            ProfileRef::Numeric(1),
            "Local model",
            "Named first turn".into(),
            report("First question", &[1]).into(),
            || Ok(()),
        )?;
        let named = load(&root, &named.id)?;
        assert!(!named.is_draft());
        assert_eq!(named.revision, 1);
        assert_eq!(named.title(), "Named first turn");
        assert_eq!(named.turns[0].question, "First question");
        Ok(())
    }

    #[test]
    fn saved_swarm_round_trips_updates_and_keeps_deleted_origins_reserved() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let initial = report("First masked question\nMore text", &[8, 3]);
        assert!(list(&root)?.chats.is_empty());
        let seed_record = seed(
            &root,
            &origin,
            ProfileRef::Numeric(10),
            "Model A",
            initial.clone(),
            || Ok(()),
        )?
        .expect("new origin");
        assert_eq!(seed_record.id, origin.id());
        assert_eq!(seed_record.revision, 1);
        assert_eq!(seed_record.target_ids().collect::<Vec<_>>(), [8, 3]);
        let initial_row = list(&root)?.chats.pop().expect("listed seed");
        assert_eq!(initial_row.title, "First masked question");
        assert_eq!(seed_record.profile_id, ProfileRef::Numeric(10));
        assert_eq!(initial_row.profile_name, "Model A");
        assert_eq!(initial_row.target_count, 2);
        assert!(initial_row.matches(&load(&root, origin.id())?));

        let followup = report("Second masked question", &[3, 12, 8]);
        let updated = append(
            &root,
            &seed_record,
            ProfileRef::Numeric(11),
            "Model B",
            followup.clone(),
            || Ok(()),
        )?;
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.profile_id, ProfileRef::Numeric(11));
        assert_eq!(updated.profile_name, "Model B");
        assert_eq!(updated.target_ids().collect::<Vec<_>>(), [3, 12, 8]);
        assert!(updated.turns[0] == Turn::from(initial.clone()));
        assert!(updated.turns[1] == Turn::from(followup.clone()));
        assert!(!initial_row.matches(&updated));
        assert!(updated.summary().updated >= initial_row.updated);
        let reopened = seed(
            &root,
            &Origin::from_id(origin.id())?,
            ProfileRef::Numeric(10),
            "Model A",
            initial.clone(),
            || Ok(()),
        )?
        .expect("same origin returns its current revision");
        assert!(reopened.same_revision(&updated));
        assert!(matches!(
            seed(&root, &origin, ProfileRef::Numeric(11), "Model B", followup.clone(), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            append(&root, &seed_record, ProfileRef::Numeric(11), "Model B", followup.clone(), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            delete(&root, &seed_record).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        delete(&root, &updated)?;
        assert!(list(&root)?.chats.is_empty());
        assert!(
            matches!(load(&root, origin.id()), Err(error) if error.kind() == io::ErrorKind::NotFound)
        );
        assert!(
            seed(
                &root,
                &origin,
                ProfileRef::Numeric(10),
                "Model A",
                initial,
                || Ok(())
            )?
            .is_none()
        );
        assert!(
            append(
                &root,
                &updated,
                ProfileRef::Numeric(11),
                "Model B",
                followup,
                || Ok(())
            )
            .is_err()
        );
        let tombstone = std::fs::read_to_string(root.join(FOLDER).join(filename(origin.id())?))?;
        let value: serde_json::Value =
            serde_json::from_str(&tombstone).map_err(io::Error::other)?;
        assert_eq!(
            value,
            serde_json::json!({"kind":"deleted", "version":1, "id":origin.id()})
        );
        Ok(())
    }

    #[test]
    fn saved_swarm_rechecks_authority_and_rejects_external_edits_with_the_same_revision()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let calls = Cell::new(0);
        let revoked = || {
            let previous = calls.replace(calls.get() + 1);
            if previous == 0 {
                Ok(())
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        };
        assert!(matches!(
            seed(&root, &origin, ProfileRef::Numeric(1), "Model", report("Seed", &[1]), revoked),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(list(&root)?.chats.is_empty());
        let initial = seed(
            &root,
            &origin,
            ProfileRef::Numeric(1),
            "Model",
            report("Seed", &[1]),
            || Ok(()),
        )?
        .expect("new origin");
        calls.set(0);
        assert!(matches!(
            append(&root, &initial, ProfileRef::Numeric(1), "Model", report("Follow-up", &[1]), revoked),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert!(initial.same_revision(&load(&root, origin.id())?));

        let directory = open(&root, false)?;
        let mut edit = initial.clone();
        edit.turns[0].targets[0].detail = "External edit".into();
        let bytes = serde_json::to_vec(&Stored::Conversation(edit)).map_err(io::Error::other)?;
        directory.replace_file(&filename(origin.id())?, &bytes)?;
        let fresh = load(&root, origin.id())?;
        assert_eq!(initial.revision, fresh.revision);
        assert!(!initial.same_revision(&fresh));
        assert!(matches!(
            append(&root, &initial, ProfileRef::Numeric(1), "Model", report("Follow-up", &[1]), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            delete(&root, &initial).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fresh.turns[0].targets[0].detail, "External edit");
        Ok(())
    }

    #[test]
    fn completed_swarm_rename_preserves_history_origin_and_requires_current_revision()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let seed_report = report("Seed title", &[1]);
        let initial = seed(
            &root,
            &origin,
            ProfileRef::Numeric(1),
            "Seed model",
            seed_report.clone(),
            || Ok(()),
        )?
        .unwrap();
        let follow_up = append(
            &root,
            &initial,
            ProfileRef::Numeric(2),
            "Follow-up model",
            report("Follow-up", &[2]),
            || Ok(()),
        )?;
        let current = set_comparison(&root, &follow_up, "Comparison evidence".into(), || Ok(()))?;
        assert_eq!(current.title(), "Seed title");

        let calls = Cell::new(0);
        let revoked = || {
            if calls.replace(calls.get() + 1) == 0 {
                Ok(())
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        };
        assert_eq!(
            rename(&root, &current, "Revoked title".into(), revoked)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(load(&root, &current.id)?.same_revision(&current));
        assert_eq!(
            rename(&root, &follow_up, "Stale title".into(), || Ok(()))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(rename(&root, &current, "line\nbreak".into(), || Ok(())).is_err());

        let renamed = rename(&root, &current, "Reviewed title".into(), || Ok(()))?;
        assert_eq!(renamed.title(), "Reviewed title");
        assert_eq!(renamed.id, origin.id());
        assert_eq!(renamed.revision, current.revision + 1);
        assert_eq!(renamed.profile_id, current.profile_id);
        assert_eq!(renamed.profile_name, current.profile_name);
        assert!(renamed.turns == current.turns);
        assert!(load(&root, &current.id)?.same_revision(&renamed));
        assert!(
            rename(&root, &renamed, "Reviewed title".into(), || Ok(()))?.same_revision(&renamed)
        );
        assert!(
            seed(
                &root,
                &origin,
                ProfileRef::Numeric(1),
                "Seed model",
                seed_report,
                || Ok(())
            )?
            .unwrap()
            .same_revision(&renamed)
        );
        Ok(())
    }

    #[test]
    fn saved_swarm_comparison_binds_exact_current_turn_and_preserves_legacy_records()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let initial = report("First masked question", &[8, 3]);
        let original = seed(
            &root,
            &origin,
            ProfileRef::Numeric(1),
            "Model",
            initial.clone(),
            || Ok(()),
        )?
        .expect("new origin");
        let path = root.join(FOLDER).join(filename(origin.id())?);
        let legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)?;
        assert_eq!(legacy["seed_profile_id"], 1);
        assert_eq!(legacy["profile_id"], 1);
        assert!(legacy["turns"][0].get("comparison").is_none());
        assert!(original.turns[0].comparison.is_none());

        let calls = Cell::new(0);
        assert!(matches!(
            set_comparison(&root, &original, "Masked comparison".into(), || {
                let previous = calls.replace(calls.get() + 1);
                if previous == 0 {
                    Ok(())
                } else {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                }
            }),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
        for invalid in [
            " ".to_owned(),
            "Invalid\u{001b}text".to_owned(),
            "x".repeat(MAX_MARKDOWN_BYTES + 1),
        ] {
            assert!(matches!(
                set_comparison(&root, &original, invalid, || Ok(())),
                Err(error) if error.kind() == io::ErrorKind::InvalidData
            ));
        }
        assert!(original.same_revision(&load(&root, origin.id())?));

        let comparison = "Masked comparison\n\nComparison details";
        let updated = set_comparison(&root, &original, comparison.into(), || Ok(()))?;
        assert_eq!(updated.revision, original.revision);
        assert!(!updated.same_revision(&original));
        assert!(!original.summary().matches(&updated));
        let reopened = load(&root, origin.id())?;
        assert!(updated.same_revision(&reopened));
        assert_eq!(reopened.turns[0].comparison.as_deref(), Some(comparison));
        let reseed = seed(
            &root,
            &origin,
            ProfileRef::Numeric(1),
            "Model",
            initial,
            || Ok(()),
        )?
        .expect("seed still resolves to its existing conversation");
        assert!(reseed.same_revision(&updated));
        for stale_or_used in [&original, &updated] {
            assert!(matches!(
                set_comparison(&root, stale_or_used, "Replacement".into(), || Ok(())),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists
            ));
        }
        assert!(matches!(
            append(&root, &original, ProfileRef::Numeric(1), "Model", report("Stale", &[3]), || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));

        let next = append(
            &root,
            &updated,
            ProfileRef::Numeric(1),
            "Model",
            report("Follow-up", &[3]),
            || Ok(()),
        )?;
        assert_eq!(next.revision, 2);
        assert_eq!(next.turns[0].comparison.as_deref(), Some(comparison));
        assert!(next.turns[1].comparison.is_none());
        let next = set_comparison(&root, &next, "Follow-up comparison".into(), || Ok(()))?;
        assert_eq!(next.turns[0].comparison.as_deref(), Some(comparison));
        assert_eq!(
            next.turns[1].comparison.as_deref(),
            Some("Follow-up comparison")
        );
        Ok(())
    }

    #[test]
    fn scheduled_comparison_replays_with_the_exact_seeded_turn() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let comparison = "| Target | Errors |\n| --- | --- |\n| First | - |\n| Second | *** |";
        let mut scheduled = report("Review the scheduled targets", &[8, 3]);
        scheduled.comparison = Some(comparison.into());
        let seed_profile = ProfileRef::Policy("policy-Seed".into());
        let follow_up_profile = ProfileRef::Policy("policy-FollowUp".into());
        let initial = seed(
            &root,
            &origin,
            seed_profile.clone(),
            "Model",
            scheduled.clone(),
            || Ok(()),
        )?
        .expect("new scheduled origin");
        let updated = append(
            &root,
            &initial,
            follow_up_profile.clone(),
            "Model",
            report("Follow-up", &[3]),
            || Ok(()),
        )?;
        let restored = load(&root, origin.id())?;
        assert!(restored.same_revision(&updated));
        assert_eq!(restored.seed_profile_id, seed_profile);
        assert_eq!(restored.profile_id, follow_up_profile);
        assert_eq!(restored.turns.len(), 2);
        assert_eq!(restored.turns[0].comparison.as_deref(), Some(comparison));
        assert!(restored.turns[1].comparison.is_none());

        let replayed = seed(
            &root,
            &origin,
            seed_profile.clone(),
            "Model",
            scheduled.clone(),
            || Ok(()),
        )?
        .expect("same origin returns its current revision");
        assert!(replayed.same_revision(&restored));
        assert!(matches!(
            seed(
                &root,
                &origin,
                ProfileRef::Policy("policy-seed".into()),
                "Model",
                scheduled.clone(),
                || Ok(())
            ),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        scheduled.comparison = Some("Different comparison".into());
        assert!(matches!(
            seed(&root, &origin, seed_profile, "Model", scheduled, || Ok(())),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists
        ));
        assert!(restored.same_revision(&load(&root, origin.id())?));
        Ok(())
    }

    #[test]
    fn saved_swarm_rejects_invalid_results_without_publishing() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let origin = Origin::new()?;
        let mut cancelled = report("Cancelled", &[1]);
        cancelled.targets[0].outcome = RunOutcome::Cancelled;
        let mut unsafe_detail = report("Unsafe detail", &[1]);
        unsafe_detail.targets[0].detail = "\u{1b}[31mnot plain text".into();
        let too_many = (1..=MAX_SWARM_TARGETS as u64 + 1).collect::<Vec<_>>();
        for rejected in [
            cancelled,
            unsafe_detail,
            report("Duplicates", &[1, 1]),
            report("Zero ID", &[0]),
            report("Empty targets", &[]),
            report("Too many targets", &too_many),
            report(&"x".repeat(MAX_SWARM_QUESTION_BYTES + 1), &[1]),
        ] {
            assert!(matches!(
                seed(&root, &origin, ProfileRef::Numeric(1), "Model", rejected, || Ok(())),
                Err(error) if error.kind() == io::ErrorKind::InvalidData
            ));
        }
        assert!(list(&root)?.chats.is_empty());
        Ok(())
    }
}
