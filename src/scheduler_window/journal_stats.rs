//! Retained Session Journal activity for saved Scheduler jobs.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kortty_jobs::JobId;

use crate::private_files::Directory;
use crate::session_journal::tree::{self, Node, NodeKey, SourceKind};
use crate::session_journal::{self, Catalog};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) type Result = std::result::Result<Arc<Report>, ()>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Request {
    root: PathBuf,
    generation: u64,
}

impl Request {
    pub(crate) fn read(&self) -> Result {
        let directory = match Directory::open(&self.root, false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Arc::new(Report::from_catalog(&Catalog::default())));
            }
            Err(_) => return Err(()),
        };
        let catalog = session_journal::catalog(&self.root).map_err(|_| ())?;
        let current = Directory::open(&self.root, false).map_err(|_| ())?;
        if !directory.same_as(&current).map_err(|_| ())? {
            return Err(());
        }
        Ok(Arc::new(Report::from_catalog(&catalog)))
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    root: Option<PathBuf>,
    active: Option<Request>,
    generation: u64,
    requested_at: Option<Instant>,
    result: Option<Result>,
}

impl Cache {
    pub(crate) fn begin(&mut self, root: Option<PathBuf>, now: Instant) -> Option<Request> {
        if self.root != root {
            self.root = root;
            self.requested_at = None;
            self.result = None;
            self.generation = self.generation.wrapping_add(1);
        }
        let root = self.root.as_ref()?;
        if self.active.is_some()
            || self
                .requested_at
                .is_some_and(|requested| now.duration_since(requested) < REFRESH_INTERVAL)
        {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let request = Request {
            root: root.clone(),
            generation: self.generation,
        };
        self.requested_at = Some(now);
        self.active = Some(request.clone());
        Some(request)
    }

    pub(crate) fn finish(&mut self, request: Request, current_root: Option<&Path>, result: Result) {
        if self.active.as_ref() != Some(&request) {
            return;
        }
        self.active = None;
        if current_root == Some(request.root.as_path()) && self.generation == request.generation {
            self.result = Some(result);
        } else if self.generation == request.generation {
            self.result = None;
            self.requested_at = None;
        }
    }

    pub(super) fn result(&self, root: &Path) -> Option<&Result> {
        (self.root.as_deref() == Some(root))
            .then_some(self.result.as_ref())
            .flatten()
    }
}

pub(crate) struct Report {
    jobs: BTreeMap<String, JobStats>,
    pub(super) incomplete: bool,
}

struct JobStats {
    runs: usize,
    bytes: u64,
    kept: Activity,
    newest: Activity,
}

struct Activity {
    journals: usize,
    tokens: u64,
    partial: bool,
    price: Option<(f64, String)>,
}

impl Report {
    fn from_catalog(catalog: &Catalog) -> Self {
        let incomplete = catalog.limited || catalog.skipped != 0;
        let jobs = tree::build_matching(catalog, |_, journal| {
            journal.source_kind == SourceKind::Job && journal.source_id.is_some()
        })
        .into_iter()
        .filter_map(|group| {
            let NodeKey::Group(key) = &group.key else {
                return None;
            };
            let id = key.source_id.as_ref()?;
            let newest = group.children.first()?;
            Some((
                id.clone(),
                JobStats {
                    runs: group.run_count,
                    bytes: group.storage_bytes,
                    kept: Activity::from_node(&group, catalog, incomplete),
                    newest: Activity::from_node(newest, catalog, incomplete),
                },
            ))
        })
        .collect();
        Self { jobs, incomplete }
    }

    pub(super) fn cell(&self, id: &JobId, enabled: bool) -> String {
        self.jobs.get(id.as_str()).map_or_else(
            || {
                if enabled && !self.incomplete {
                    "0".to_owned()
                } else {
                    String::new()
                }
            },
            |job| job.kept.usage(false),
        )
    }

    pub(super) fn details(&self, id: &JobId, enabled: bool) -> Option<String> {
        if let Some(job) = self.jobs.get(id.as_str()) {
            let newest = if self.incomplete {
                "Latest found run"
            } else {
                "Last retained run"
            };
            let journals = job.newest.journals;
            let runs = job.runs;
            return Some(format!(
                "{newest} · {journals} journal{} · {}\nKept · {runs} run{} · {} · {:.1} MiB",
                if journals == 1 { "" } else { "s" },
                job.newest.usage(true),
                if runs == 1 { "" } else { "s" },
                job.kept.usage(true),
                job.bytes as f64 / (1024.0 * 1024.0),
            ));
        }
        (enabled && !self.incomplete).then(|| "No retained Session Journals for this job.".into())
    }
}

impl Activity {
    fn from_node(node: &Node, catalog: &Catalog, incomplete: bool) -> Self {
        let partial = incomplete
            || node
                .journals
                .iter()
                .any(|&index| catalog.journals[index].usage_partial);
        Self {
            journals: node.journal_count(),
            tokens: node.usage.total_tokens,
            partial,
            price: if partial {
                None
            } else {
                Self::price(node, catalog)
            },
        }
    }

    fn price(node: &Node, catalog: &Catalog) -> Option<(f64, String)> {
        let mut currency = None;
        let mut price = 0.0;
        for &index in &node.journals {
            let usage = &catalog.journals[index].usage;
            if usage.local
                || (usage.call_count == 0 && usage.total_tokens == 0 && usage.cost == 0.0)
            {
                continue;
            }
            let current = usage.currency.as_deref()?;
            if usage.cost < 0.0
                || current.len() != 3
                || !current.bytes().all(|byte| byte.is_ascii_uppercase())
                || currency.is_some_and(|currency| currency != current)
            {
                return None;
            }
            currency = Some(current);
            price += usage.cost;
        }
        (price > 0.0 && price.is_finite()).then_some((price, currency?.to_owned()))
    }

    fn usage(&self, show_unit: bool) -> String {
        let tokens = self.tokens;
        let mut label = match tokens {
            1_000_000.. => format!("{:.1}M", tokens as f64 / 1_000_000.0),
            1_000.. => format!("{:.1}k", tokens as f64 / 1_000.0),
            _ => tokens.to_string(),
        };
        if show_unit {
            label.push_str(" AI tokens");
        }
        if self.partial {
            label.push_str(" (partial)");
        }
        if let Some((price, currency)) = &self.price {
            label.push_str(&format!(" · ≈ {price:.4} {currency}"));
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_journal(
        root: &Path,
        id: &str,
        (kind, job): (&str, &str),
        (run, day): (&str, u8),
        (tokens, cost, currency, partial): (u64, f64, &str, bool),
    ) {
        let folder = root.join(id);
        fs::create_dir(&folder).unwrap();
        fs::write(
            folder.join("journal.xml"),
            format!(
                r#"<session-journal formatVersion="1" id="{id}"><meta>
                   <sourceKind>{kind}</sourceKind><sourceId>{job}</sourceId><runId>{run}</runId>
                   <runStartedAt>2026-11-{day:02}T00:00:00Z</runStartedAt>
                   <startedAt>2026-11-{day:02}T00:01:00Z</startedAt><storageBytes>1048576</storageBytes>
                   <aiCallCount>1</aiCallCount><aiPromptTokens>{tokens}</aiPromptTokens>
                   <aiCompletionTokens>0</aiCompletionTokens><aiTotalTokens>{tokens}</aiTotalTokens>
                   <aiCost>{cost}</aiCost><aiCostCurrency>{currency}</aiCostCurrency>
                   <aiUsagePartial>{partial}</aiUsagePartial>
                   </meta><entries/></session-journal>"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn scheduler_journal_stats_reads_kept_jobs_and_preserves_currency_boundaries() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        write_journal(
            &root,
            "old",
            ("JOB", "backup"),
            ("a", 1),
            (100, 0.1, "USD", false),
        );
        write_journal(
            &root,
            "new1",
            ("JOB", "backup"),
            ("b", 2),
            (200, 0.2, "EUR", false),
        );
        write_journal(
            &root,
            "new2",
            ("JOB", "backup"),
            ("b", 2),
            (300, 0.3, "EUR", false),
        );
        write_journal(
            &root,
            "other",
            ("JOB", "backup-copy"),
            ("b", 2),
            (900, 0.9, "USD", false),
        );
        write_journal(
            &root,
            "swarm",
            ("SWARM", "backup"),
            ("b", 2),
            (900, 0.9, "USD", false),
        );
        let mut cache = Cache::default();
        let request = cache.begin(Some(root), Instant::now()).unwrap();
        let report = request.read().unwrap();
        let job = JobId::new("backup").unwrap();
        let empty = JobId::new("empty").unwrap();
        assert_eq!(report.cell(&job, false), "600");
        assert_eq!(
            report.details(&job, false).unwrap(),
            "Last retained run · 2 journals · 500 AI tokens · ≈ 0.5000 EUR\nKept · 2 runs · 600 AI tokens · 3.0 MiB"
        );
        assert_eq!(report.cell(&empty, true), "0");
        assert!(report.cell(&empty, false).is_empty());
    }

    #[test]
    fn scheduler_journal_stats_labels_partial_metadata_and_unknown_jobs() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        write_journal(
            &root,
            "partial",
            ("JOB", "backup"),
            ("a", 1),
            (7, 0.1, "USD", true),
        );
        let broken = root.join("broken");
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join("journal.xml"), "invalid").unwrap();
        let mut cache = Cache::default();
        let request = cache.begin(Some(root), Instant::now()).unwrap();
        let report = request.read().unwrap();
        let job = JobId::new("backup").unwrap();
        let empty = JobId::new("empty").unwrap();
        assert!(report.incomplete);
        assert_eq!(report.cell(&job, true), "7 (partial)");
        assert!(
            report
                .details(&job, true)
                .unwrap()
                .starts_with("Latest found run")
        );
        assert!(report.cell(&empty, true).is_empty());
        assert!(report.details(&empty, true).is_none());
    }

    #[test]
    fn scheduler_journal_stats_cache_rejects_late_results_and_limits_refreshes() {
        let mut cache = Cache::default();
        let now = Instant::now();
        let first = PathBuf::from("first");
        let second = PathBuf::from("second");
        let request = cache.begin(Some(first.clone()), now).unwrap();
        assert!(cache.begin(Some(second.clone()), now).is_none());
        assert!(cache.begin(Some(first.clone()), now).is_none());
        cache.finish(request.clone(), Some(&first), Err(()));
        assert!(cache.result(&first).is_none());
        let new_request = cache.begin(Some(first.clone()), now).unwrap();
        cache.finish(request, Some(&first), Err(()));
        assert!(cache.result(&first).is_none());
        cache.finish(new_request, Some(&first), Err(()));
        assert!(matches!(cache.result(&first), Some(Err(()))));
        assert!(cache.result(&second).is_none());
        assert!(cache.begin(Some(first.clone()), now).is_none());
        assert!(cache.begin(Some(first), now + REFRESH_INTERVAL).is_some());
    }
}
