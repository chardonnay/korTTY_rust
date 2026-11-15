use super::replacement::ScheduledReplacement;
use super::*;

pub(super) struct Replacements {
    to: Side,
    standing: Option<StandingLimits>,
    entries: Vec<(Option<Relative>, ScheduledReplacement)>,
    pub(super) bytes: u64,
}

impl Replacements {
    pub(super) fn new(to: Side, standing: Option<StandingLimits>) -> Self {
        Self {
            to,
            standing,
            entries: Vec::new(),
            bytes: 0,
        }
    }

    pub(super) fn proof_domain(&self) -> &'static [u8] {
        ScheduledReplacement::proof_domain(self.to)
    }

    pub(super) fn prepare(
        &mut self,
        client: &mut Client,
        review: &Review,
        new_actions: usize,
        new_bytes: u64,
        field: &mut impl FnMut(&[u8]),
        cancelled: &impl Fn() -> bool,
    ) -> Result<()> {
        let (source, target) = review.direction(self.to).ok_or_else(|| {
            io::Error::other("Scheduled replacement requires a local and a server folder")
        })?;
        let mut scanned = [new_bytes, 0];
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut labels = 0;
        for (index, path) in review.paired_regular_files().enumerate() {
            require_running(cancelled)?;
            if index == MAX_ACTIONS {
                return Err(io::Error::other(
                    "Scheduled replacement compares up to 256 matching files",
                )
                .into());
            }
            let sizes = [source, target].map(|manifest| {
                manifest
                    .entries
                    .get(path)
                    .and_then(SnapshotEntry::file_metadata)
                    .map(|(bytes, _)| bytes)
            });
            let [Some(source_size), Some(target_size)] = sizes else {
                return Err(io::Error::other("A paired file has incomplete metadata").into());
            };
            for (total, size) in scanned.iter_mut().zip([source_size, target_size]) {
                *total = total
                    .checked_add(size)
                    .filter(|bytes| *bytes <= MAX_STANDING_BYTES)
                    .ok_or_else(|| {
                        io::Error::other("Scheduled replacement review exceeds 256 MiB on one side")
                    })?;
            }
            let entry = ScheduledReplacement::prepare(client, review, path, self.to, cancelled)?;
            field(path.bytes());
            entry.bind(field)?;
            if entry.differs() {
                if new_actions + entries.len()
                    >= self.standing.map_or(MAX_ACTIONS, |limits| limits.actions)
                {
                    return Err(io::Error::other(if self.standing.is_some() {
                        "Standing copy exceeds the per-firing file and folder limit"
                    } else {
                        "Scheduled copy exceeds 256 files and folders"
                    })
                    .into());
                }
                bytes += source_size;
                if self
                    .standing
                    .is_some_and(|limits| new_bytes + bytes > limits.bytes)
                {
                    return Err(io::Error::other(
                        "Standing copy exceeds the per-firing byte limit",
                    )
                    .into());
                }
                for label in entry.labels() {
                    labels += label.len();
                    if label.len() > MAX_LABEL_BYTES || labels > MAX_REVIEW_LABEL_BYTES {
                        return Err(io::Error::other(
                            "The exact replacement paths exceed the review display limit",
                        )
                        .into());
                    }
                }
                entries.push((path.parent(), entry));
            }
        }
        if new_actions == 0 && entries.is_empty() && self.standing.is_none() {
            let direction = if self.to == Side::Local {
                "download"
            } else {
                "upload"
            };
            return Err(io::Error::other(format!(
                "There are no missing or changed regular files to {direction}"
            ))
            .into());
        }
        self.entries = entries;
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn preview(&self) -> Vec<ReplacementPreview> {
        self.entries
            .iter()
            .map(|(_, entry)| {
                let [source, target] = entry.labels();
                let [source_sha256, target_sha256] = entry.digests();
                ReplacementPreview {
                    source: source.to_owned(),
                    target: target.to_owned(),
                    source_sha256: source_sha256.to_owned(),
                    target_sha256: target_sha256.to_owned(),
                }
            })
            .collect()
    }

    pub(super) fn count(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn apply(
        &self,
        client: &mut Client,
        progress: &Progress,
        checks: &WriteChecks<'_>,
        applied: &mut Applied,
        parents: &mut BTreeMap<Option<Relative>, DirectoryStamp>,
    ) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let nonce = crate::private_files::nonce()?;
        let prefix = &nonce[..16];
        applied.recovery_prefix = Some(format!(".kortty-{prefix}????????????????.kortty-part"));
        for (parent, entry) in &self.entries {
            checks.before_write(client)?;
            let warning = entry
                .apply(client, progress, checks, prefix)
                .map_err(|error| error.context(format!(" · {}", entry.labels()[1])))?;
            applied.replaced += 1;
            if let Some(warning) = warning {
                return Err(warning.context(format!(" · {}", entry.labels()[1])));
            }
            if self.to == Side::Remote {
                let current = entry.refreshed_remote_parent(client)?;
                parents.insert(parent.clone(), current);
            }
        }
        Ok(())
    }
}
