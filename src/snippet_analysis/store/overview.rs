//! Compact library status derived from persisted analysis histories.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::{fmt, io, sync::Arc};

use super::{
    FileStamp, History, Store, Subject, content_hash, file_name, read_stamped, require_single_link,
};
use crate::snippet_analysis::model::{ApplyStatus, java_history::Outcome};
use crate::snippets::{Library, SavedApply, Snippet, SnippetId};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Kind {
    Review,
    Findings,
    Applied,
    Clean,
    #[default]
    None,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Status {
    pub(crate) kind: Kind,
    pub(crate) open: usize,
    pub(crate) stale: bool,
    pub(crate) unsaved: bool,
}

impl Status {
    pub(crate) fn sort_rank(self) -> usize {
        match self.kind {
            Kind::Review => 0,
            _ if self.unsaved => 1,
            Kind::Findings => 1_000_000 - self.open.min(999_998),
            Kind::Applied => 2_000_000,
            Kind::Clean => 2_000_001,
            Kind::Unavailable => usize::MAX - 1,
            Kind::None => usize::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Filter {
    #[default]
    All,
    Open,
    Stale,
    Review,
}

impl Filter {
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Open, Self::Stale, Self::Review];

    pub(crate) fn matches(self, status: Status) -> bool {
        match self {
            Self::All => true,
            Self::Open => status.open > 0,
            Self::Stale => status.stale,
            Self::Review => status.kind == Kind::Review || status.unsaved,
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::All => "All analysis statuses",
            Self::Open => "Open findings",
            Self::Stale => "Stale analyses",
            Self::Review => "Pending reviews",
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Entry {
    file: Option<FileStamp>,
    source: Arc<str>,
    versions: Vec<Arc<str>>,
    saved_results: BTreeSet<SavedApply>,
    status: Status,
}

#[derive(Default, Eq, PartialEq)]
pub(crate) struct Overview(HashMap<SnippetId, Entry>);

impl Overview {
    pub(crate) fn status(&self, snippet: &Snippet) -> Status {
        self.0
            .get(&snippet.id())
            .filter(|entry| {
                entry.source.as_ref() == snippet.content()
                    && &entry.saved_results == snippet.saved_apply_results()
            })
            .map_or(Status::default(), |entry| entry.status)
    }
}

impl Store {
    pub(crate) fn library_overview(
        &self,
        library: &Library,
        previous: &Overview,
    ) -> io::Result<Overview> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Overview::default()),
            Err(error) => return Err(error),
        };
        let mut next = HashMap::new();
        for snippet in library.entries() {
            let subject = Subject::Snippet(snippet.id());
            let name = file_name(subject)?;
            let load = || -> io::Result<Option<Entry>> {
                let file = match directory.open_private_file(&name) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error),
                };
                require_single_link(&file)?;
                let stamp = FileStamp::read(&file)?;
                if let Some(entry) = previous.0.get(&snippet.id()).filter(|entry| {
                    entry.file.as_ref() == Some(&stamp)
                        && entry.source.as_ref() == snippet.content()
                        && &entry.saved_results == snippet.saved_apply_results()
                        && entry.versions.iter().map(AsRef::as_ref).eq(snippet
                            .history()
                            .iter()
                            .map(|revision| revision.content.as_ref()))
                }) {
                    return Ok(Some(entry.clone()));
                }
                let history = read_stamped(&file, &stamp, subject)?;
                Ok(Some(Entry {
                    file: Some(stamp),
                    source: snippet.shared_content(),
                    versions: snippet
                        .history()
                        .iter()
                        .map(|revision| revision.content.clone())
                        .collect(),
                    saved_results: snippet.saved_apply_results().clone(),
                    status: summarize(&history, snippet)?,
                }))
            };
            match load() {
                Ok(Some(entry)) => {
                    next.insert(snippet.id(), entry);
                }
                Ok(None) => {}
                Err(_) => {
                    next.insert(
                        snippet.id(),
                        Entry {
                            file: None,
                            source: snippet.shared_content(),
                            versions: Vec::new(),
                            saved_results: snippet.saved_apply_results().clone(),
                            status: Status {
                                kind: Kind::Unavailable,
                                ..Status::default()
                            },
                        },
                    );
                }
            }
        }
        Ok(Overview(next))
    }
}

fn summarize(history: &History, snippet: &Snippet) -> io::Result<Status> {
    let Some(current) = history.current() else {
        return Ok(Status::default());
    };
    let saved = content_hash(snippet.content())?;
    let mut status = Status::default();
    let mut applied = HashSet::new();
    let mut saved_result = false;
    let mut matches_current = false;
    let mut pending = false;
    let mut version_hashes = None;
    for record in &history.records {
        let latest = record.id == current.id;
        for run in &record.apply_runs {
            pending |= run.status == ApplyStatus::Proposed;
            if run.status != ApplyStatus::Accepted {
                continue;
            }
            let matches = run.after_sha256 == saved;
            let was_saved = if matches
                || snippet.saved_apply_results().contains(&SavedApply {
                    report: record.id,
                    run: run.id,
                }) {
                true
            } else {
                if version_hashes.is_none() {
                    version_hashes = Some(
                        snippet
                            .history()
                            .iter()
                            .map(|version| content_hash(&version.content))
                            .collect::<io::Result<HashSet<_>>>()?,
                    );
                }
                version_hashes
                    .as_ref()
                    .is_some_and(|versions| versions.contains(&run.after_sha256))
            };
            status.unsaved |= !was_saved && run.has_retained_result();
            if latest {
                matches_current |= matches;
                if was_saved {
                    saved_result = true;
                    applied.extend(
                        run.selected
                            .findings
                            .iter()
                            .chain(&run.selected.dependencies),
                    );
                }
            }
        }
        if let Some(java) = &record.java_history {
            for run in &java.apply_runs {
                pending |= run.outcome == Outcome::PendingReview;
                if run.outcome != Outcome::Accepted {
                    continue;
                }
                let matches = run.accepted_content_sha256 == saved;
                let was_saved = run.saved_to_snippet_at != 0 || matches;
                status.unsaved |=
                    !was_saved && java.attachments.contains_key(&run.accepted_content_sha256);
                if latest {
                    matches_current |= matches;
                    if was_saved {
                        saved_result = true;
                        applied.extend(&run.applied_finding_ids);
                    }
                }
            }
        }
    }
    let ids: HashSet<_> = current
        .analysis
        .improvements
        .iter()
        .map(|finding| &finding.id)
        .chain(
            current
                .analysis
                .dependencies
                .iter()
                .map(|dependency| &dependency.id),
        )
        .collect();
    status.open = ids.difference(&applied).count();
    status.stale =
        !current.source.sha256.is_empty() && current.source.sha256 != saved && !matches_current;
    status.kind = if pending {
        Kind::Review
    } else if status.open != 0 {
        Kind::Findings
    } else if saved_result {
        Kind::Applied
    } else {
        Kind::Clean
    };
    Ok(status)
}
