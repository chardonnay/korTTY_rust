use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::{Endpoint, HostKey, Pins, TrustStore, TrustedHost, invalid};

const MAX_CANDIDATES: usize = 16_384;
const MAX_HOST_FIELD_BYTES: usize = 32_768;
const MAX_HOSTS_PER_LINE: usize = 256;
const MAX_REPORTED_MALFORMED: usize = 256;
const NEGOTIATED_KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
];

/// A bounded snapshot of importable OpenSSH entries and their diagnostics.
#[derive(Clone, Debug)]
pub struct KnownHostsFile {
    candidates: BTreeMap<Endpoint, BTreeMap<String, Candidate>>,
    revoked: BTreeSet<String>,
    skipped: ImportSkipped,
}

#[derive(Clone, Debug)]
struct Candidate {
    key: HostKey,
    line: usize,
    rank: usize,
}

/// Diagnostics for OpenSSH syntax and entries the transport cannot import as pins.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportSkipped {
    pub hashed: usize,
    pub revoked: usize,
    pub authorities: usize,
    pub patterns: usize,
    pub unsupported: usize,
    pub malformed: usize,
    pub malformed_lines: Vec<usize>,
    pub revoked_candidates: usize,
}

/// A reviewed proposal from an immutable source snapshot.
#[derive(Clone, Debug)]
pub struct ImportReview {
    source: KnownHostsFile,
    report: ImportReport,
}

impl ImportReview {
    pub fn report(&self) -> &ImportReport {
        &self.report
    }
}

/// The outcome of a preview or the actual atomic import.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportReport {
    pub added: Vec<TrustedHost>,
    pub already_trusted: usize,
    pub conflicts: Vec<ImportConflict>,
    pub trusted_but_revoked: Vec<TrustedHost>,
    pub additional_keys: usize,
    /// Endpoints that became available after preview and need a new approval.
    pub needs_review: usize,
    pub skipped: ImportSkipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportConflict {
    pub endpoint: Endpoint,
    pub trusted: HostKey,
    pub offered: HostKey,
    pub line: usize,
}

impl KnownHostsFile {
    pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

    /// Parses OpenSSH host lists and records skipped lines without changing trust.
    pub fn parse(input: &str) -> io::Result<Self> {
        if input.len() as u64 > Self::MAX_BYTES {
            return Err(invalid("known_hosts input exceeds 8 MiB"));
        }
        let mut source = Self {
            candidates: BTreeMap::new(),
            revoked: BTreeSet::new(),
            skipped: ImportSkipped::default(),
        };
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        let mut candidates = 0;
        for (offset, line) in input.lines().enumerate() {
            if source
                .parse_line(line.trim(), offset + 1, &mut candidates)
                .is_err()
            {
                return Err(invalid("known_hosts contains too many host-key candidates"));
            }
        }
        for keys in source.candidates.values_mut() {
            let count = keys.len();
            keys.retain(|fingerprint, _| !source.revoked.contains(fingerprint));
            source.skipped.revoked_candidates += count - keys.len();
        }
        source.candidates.retain(|_, keys| !keys.is_empty());
        Ok(source)
    }

    fn parse_line(&mut self, line: &str, number: usize, total: &mut usize) -> Result<(), ()> {
        if line.is_empty() || line.starts_with('#') {
            return Ok(());
        }
        let mut fields = line.split_ascii_whitespace();
        let Some(first) = fields.next() else {
            return Ok(());
        };
        let marker = first.starts_with('@').then_some(first);
        let hosts = if marker.is_some() {
            fields.next()
        } else {
            Some(first)
        };
        let (Some(hosts), Some(algorithm), Some(key_text)) = (hosts, fields.next(), fields.next())
        else {
            self.malformed(number);
            return Ok(());
        };
        match marker {
            Some("@cert-authority") => {
                self.skipped.authorities += 1;
                return Ok(());
            }
            Some("@revoked") => {
                self.skipped.revoked += 1;
                if let Ok(key) = parse_key(algorithm, key_text) {
                    self.revoked.insert(key.fingerprint());
                    if self.revoked.len() > MAX_CANDIDATES {
                        return Err(());
                    }
                } else if NEGOTIATED_KEY_TYPES.contains(&algorithm) {
                    self.malformed(number);
                }
                return Ok(());
            }
            Some(_) => {
                self.malformed(number);
                return Ok(());
            }
            None => {}
        }
        if hosts.starts_with('|') {
            self.skipped.hashed += 1;
            return Ok(());
        }
        if hosts.len() > MAX_HOST_FIELD_BYTES {
            self.malformed(number);
            return Ok(());
        }
        let mut literal = Vec::new();
        let mut negations = Vec::new();
        for (index, token) in hosts.split(',').enumerate() {
            if index >= MAX_HOSTS_PER_LINE || token.len() > 262 {
                self.malformed(number);
                return Ok(());
            }
            if let Some(pattern) = token.strip_prefix('!') {
                if pattern.is_empty() {
                    self.malformed(number);
                    return Ok(());
                }
                self.skipped.patterns += 1;
                negations.push(pattern.to_ascii_lowercase());
            } else if token.contains(['*', '?']) {
                self.skipped.patterns += 1;
            } else {
                let Ok(endpoint) = Endpoint::parse_known_host(token) else {
                    self.malformed(number);
                    return Ok(());
                };
                literal.push((token, endpoint));
            }
        }
        if literal.is_empty() {
            return Ok(());
        }
        let Some(rank) = NEGOTIATED_KEY_TYPES
            .iter()
            .position(|key_type| *key_type == algorithm)
        else {
            self.skipped.unsupported += 1;
            return Ok(());
        };
        let Ok(key) = parse_key(algorithm, key_text) else {
            self.malformed(number);
            return Ok(());
        };
        *total += literal.len();
        if *total > MAX_CANDIDATES {
            return Err(());
        }
        let fingerprint = key.fingerprint();
        for (token, endpoint) in literal {
            let wire_name = token.to_ascii_lowercase();
            let canonical = endpoint.to_string();
            if negations.iter().any(|pattern| {
                if pattern.contains(['*', '?']) {
                    wildcard_match(pattern.as_bytes(), canonical.as_bytes())
                        || wildcard_match(pattern.as_bytes(), wire_name.as_bytes())
                } else {
                    Endpoint::parse_known_host(pattern).is_ok_and(|excluded| excluded == endpoint)
                }
            }) {
                self.skipped.patterns += 1;
                continue;
            }
            self.candidates
                .entry(endpoint)
                .or_default()
                .entry(fingerprint.clone())
                .or_insert_with(|| Candidate {
                    key: key.clone(),
                    line: number,
                    rank,
                });
        }
        Ok(())
    }

    fn malformed(&mut self, line: usize) {
        self.skipped.malformed += 1;
        if self.skipped.malformed_lines.len() < MAX_REPORTED_MALFORMED {
            self.skipped.malformed_lines.push(line);
        }
    }

    fn classify(&self, pins: &Pins, reviewed: Option<&ImportReport>) -> ImportReport {
        self.classify_selected(pins, reviewed, None)
    }

    fn classify_selected(
        &self,
        pins: &Pins,
        reviewed: Option<&ImportReport>,
        selection: Option<&BTreeSet<usize>>,
    ) -> ImportReport {
        let mut report = ImportReport {
            skipped: self.skipped.clone(),
            ..ImportReport::default()
        };
        let proposed: BTreeMap<_, _> = reviewed
            .into_iter()
            .flat_map(|report| &report.added)
            .enumerate()
            .map(|(index, host)| (&host.endpoint, (index, &host.key)))
            .collect();
        for (endpoint, candidates) in &self.candidates {
            let Some(preferred) = candidates
                .values()
                .min_by_key(|candidate| (candidate.rank, candidate.line))
            else {
                continue;
            };
            if let Some(trusted) = pins.get(endpoint) {
                if candidates
                    .values()
                    .any(|candidate| candidate.key == trusted.key)
                {
                    report.already_trusted += 1;
                } else {
                    report.conflicts.push(ImportConflict {
                        endpoint: endpoint.clone(),
                        trusted: trusted.key.clone(),
                        offered: preferred.key.clone(),
                        line: preferred.line,
                    });
                }
            } else {
                if reviewed.is_some() {
                    let Some(&(index, key)) = proposed.get(endpoint) else {
                        report.needs_review += 1;
                        continue;
                    };
                    if selection.is_some_and(|selected| !selected.contains(&index)) {
                        continue;
                    }
                    if key != &preferred.key {
                        report.needs_review += 1;
                        continue;
                    }
                }
                report.added.push(TrustedHost {
                    endpoint: endpoint.clone(),
                    key: preferred.key.clone(),
                    trusted_since: None,
                });
                report.additional_keys += candidates.len() - 1;
            }
        }
        if !self.revoked.is_empty() {
            report.trusted_but_revoked.extend(
                pins.iter()
                    .filter(|(_, pin)| self.revoked.contains(&pin.key.fingerprint()))
                    .map(|(endpoint, pin)| pin.at(endpoint)),
            );
        }
        report
    }
}

impl TrustStore {
    /// Classifies an import without writing the store.
    pub fn preview_import(&self, source: KnownHostsFile) -> io::Result<ImportReview> {
        let pins = self.read_pins()?;
        let report = source.classify(&pins, None);
        let mut prospective = pins.into_owned();
        for host in &report.added {
            prospective.insert(host.endpoint.clone(), super::Pin::now(host.key.clone()));
        }
        Self::encode(&prospective)?;
        Ok(ImportReview { source, report })
    }

    /// Adds only approved keys that still have no pin, using the reviewed source bytes.
    ///
    /// Conflicts are reported and never overwrite the current key. If an endpoint becomes
    /// eligible after approval, the new report requests a separate review for that endpoint.
    pub fn import_reviewed(&self, review: &ImportReview) -> io::Result<ImportReport> {
        self.apply_review(review, None)
    }

    /// Adds only the explicitly selected positions from the review's `added` list.
    /// Each position still refers to the exact key and endpoint originally displayed.
    pub fn import_reviewed_selected(
        &self,
        review: &ImportReview,
        selected: &BTreeSet<usize>,
    ) -> io::Result<ImportReport> {
        if selected.is_empty()
            || selected
                .last()
                .is_some_and(|last| *last >= review.report.added.len())
        {
            return Err(invalid("invalid SSH host key selection"));
        }
        self.apply_review(review, Some(selected))
    }

    fn apply_review(
        &self,
        review: &ImportReview,
        selection: Option<&BTreeSet<usize>>,
    ) -> io::Result<ImportReport> {
        let path = self.writable_path()?;
        let _lock = Self::lock(path)?;
        let mut pins = Self::load(path)?;
        let mut report = review
            .source
            .classify_selected(&pins, Some(&review.report), selection);
        if !report.added.is_empty() {
            add_pins(&mut pins, &mut report.added);
            Self::write(path, &pins)?;
        }
        Ok(report)
    }
}

fn add_pins(pins: &mut Pins, hosts: &mut [TrustedHost]) {
    for host in hosts {
        let pin = super::Pin::now(host.key.clone());
        host.trusted_since = pin.since;
        pins.insert(host.endpoint.clone(), pin);
    }
}

fn parse_key(algorithm: &str, key_text: &str) -> io::Result<HostKey> {
    if algorithm.len().saturating_add(key_text.len()) >= super::MAX_KEY_LINE_BYTES {
        return Err(invalid("known_hosts key is too large"));
    }
    HostKey::from_openssh(&format!("{algorithm} {key_text}"))
}

fn wildcard_match(mut pattern: &[u8], mut text: &[u8]) -> bool {
    let mut star = None;
    while let Some((&byte, remaining)) = text.split_first() {
        match pattern.split_first() {
            Some((&b'*', tail)) => {
                pattern = tail;
                star = Some((pattern, text));
            }
            Some((&candidate, tail)) if candidate == b'?' || candidate == byte => {
                pattern = tail;
                text = remaining;
            }
            _ => {
                let Some((after_star, previous)) = star else {
                    return false;
                };
                let Some((_, rest)) = previous.split_first() else {
                    return false;
                };
                pattern = after_star;
                text = rest;
                star = Some((pattern, text));
            }
        }
    }
    pattern.iter().all(|&token| token == b'*')
}
