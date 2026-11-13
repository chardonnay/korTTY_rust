//! Host keys written by the Java kortty 2.26.0 trust store.

use std::borrow::Cow;
use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt::Write as _;
use std::io;

use jiff::Timestamp;
use kortty_ssh::{Endpoint, HostKey, KnownHostsFile};

use crate::ssh_dialog::normalize_host;

const MAX_BYTES: usize = KnownHostsFile::MAX_BYTES as usize;
const MAX_ENTRIES: usize = 100_000;
const SUPPORTED_ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
];

pub(super) struct Candidate {
    pub(super) endpoint: Endpoint,
    pub(super) key: HostKey,
    pub(super) trusted_since: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Omission {
    IncompleteRecord,
    MalformedRecord,
    UnsupportedAlgorithm,
    UnsupportedEndpoint,
    InconsistentKey,
    DuplicateAlias,
    ConflictingAlias,
}

#[derive(Default)]
pub(super) struct Decoded {
    pub(super) candidates: Vec<Candidate>,
    pub(super) omissions: BTreeMap<Omission, usize>,
    pub(super) alias_conflicts: usize,
}

#[derive(Default)]
struct Record<'a> {
    host: Option<&'a str>,
    port: Option<&'a str>,
    algorithm: Option<&'a str>,
    fingerprint: Option<&'a str>,
    public_key: Option<&'a str>,
    trusted_at: Option<&'a str>,
}

/// Reads the exact Java Properties.store(Writer) representation. This phase never
/// changes local trust, even when the archive has authenticated encryption.
pub(super) fn decode(bytes: &[u8]) -> io::Result<Decoded> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Java SSH host key store exceeds its size limit."));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| invalid("Java SSH host key store must use UTF-8."))?;
    let mut format = None;
    let mut count = None;
    let mut records = BTreeMap::<usize, Record<'_>>::new();
    for line in text.split(['\r', '\n']) {
        let line = line.trim_start_matches([' ', '\t', '\x0c']);
        if line.is_empty() || line.starts_with(['#', '!']) {
            continue;
        }
        let (name, value) = line.split_once('=').ok_or_else(invalid_property)?;
        let field = match name {
            "format" => &mut format,
            "entry.count" => &mut count,
            _ => {
                let (index, field) = name
                    .strip_prefix("entry.")
                    .and_then(|name| name.split_once('.'))
                    .ok_or_else(invalid_property)?;
                let index = entry_index(index).ok_or_else(invalid_property)?;
                records.entry(index).or_default().field(field)?
            }
        };
        if field.replace(value).is_some() {
            return Err(invalid("Duplicate property in Java SSH host key store."));
        }
    }
    if format != Some("1") {
        return Err(invalid("Unsupported Java SSH host key store format."));
    }
    let count = count
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| (1..=MAX_ENTRIES).contains(count))
        .ok_or_else(|| invalid("Invalid Java SSH host key entry count."))?;
    if records
        .last_key_value()
        .is_some_and(|(last, _)| *last >= count)
    {
        return Err(invalid(
            "Java SSH host key store contains uncounted entries.",
        ));
    }

    let mut result = Decoded::default();
    if records.len() < count {
        result
            .omissions
            .insert(Omission::IncompleteRecord, count - records.len());
    }
    let mut candidates = BTreeMap::<Endpoint, Option<Candidate>>::new();
    for record in records.into_values() {
        let candidate = match record.candidate() {
            Ok(candidate) => candidate,
            Err(reason) => {
                result.omit(reason, 1);
                continue;
            }
        };
        match candidates.entry(candidate.endpoint.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(Some(candidate));
            }
            Entry::Occupied(mut slot) => match slot.get_mut() {
                Some(existing) if existing.key == candidate.key => {
                    existing.trusted_since = match (existing.trusted_since, candidate.trusted_since)
                    {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        (left, right) => left.or(right),
                    };
                    result.omit(Omission::DuplicateAlias, 1);
                }
                Some(_) => {
                    slot.insert(None);
                    result.alias_conflicts += 1;
                    result.omit(Omission::ConflictingAlias, 2);
                }
                None => result.omit(Omission::ConflictingAlias, 1),
            },
        }
    }
    result.candidates = candidates.into_values().flatten().collect();
    Ok(result)
}

impl Decoded {
    pub(super) fn as_known_hosts(&self) -> io::Result<KnownHostsFile> {
        let mut source = String::new();
        for candidate in &self.candidates {
            writeln!(
                source,
                "{} {}",
                candidate.endpoint,
                candidate.key.to_openssh()?
            )
            .map_err(io::Error::other)?;
            if let Some(since) = candidate.trusted_since {
                // Keep source provenance as a comment. The native store records approval time.
                writeln!(source, "# Java trusted since {since}").map_err(io::Error::other)?;
            }
        }
        KnownHostsFile::parse(&source)
    }

    fn omit(&mut self, reason: Omission, count: usize) {
        *self.omissions.entry(reason).or_default() += count;
    }
}

impl<'a> Record<'a> {
    fn field(&mut self, field: &str) -> io::Result<&mut Option<&'a str>> {
        match field {
            "host" => Ok(&mut self.host),
            "port" => Ok(&mut self.port),
            "algorithm" => Ok(&mut self.algorithm),
            "fingerprintSha256" => Ok(&mut self.fingerprint),
            "publicKeyLine" => Ok(&mut self.public_key),
            "trustedAt" => Ok(&mut self.trusted_at),
            _ => Err(invalid_property()),
        }
    }

    fn candidate(self) -> Result<Candidate, Omission> {
        let host = property_value(self.host)?;
        let port = property_value(self.port)?;
        let algorithm = property_value(self.algorithm)?;
        let fingerprint = property_value(self.fingerprint)?;
        let public_key = property_value(self.public_key)?;
        let trusted_at = property_value(self.trusted_at)?;
        let port = port.parse::<u16>().map_err(|_| Omission::MalformedRecord)?;
        if port == 0 {
            return Err(Omission::MalformedRecord);
        }
        // Java removes brackets and DNS root dots in its trust identity. Rust
        // additionally canonicalizes numeric addresses and validates DNS labels.
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(&host)
            .trim_end_matches('.');
        let host = normalize_host(host).map_err(|_| Omission::UnsupportedEndpoint)?;
        let endpoint = Endpoint::new(&host, port).map_err(|_| Omission::UnsupportedEndpoint)?;
        if !SUPPORTED_ALGORITHMS.contains(&algorithm.as_ref()) {
            return Err(Omission::UnsupportedAlgorithm);
        }
        let key = HostKey::from_openssh(&public_key).map_err(|_| Omission::MalformedRecord)?;
        if key.algorithm() != algorithm.as_ref()
            || key.fingerprint() != fingerprint.as_ref()
            || key.to_openssh().map_err(|_| Omission::MalformedRecord)? != public_key.as_ref()
        {
            return Err(Omission::InconsistentKey);
        }
        let trusted_at = trusted_at
            .parse::<Timestamp>()
            .map_err(|_| Omission::MalformedRecord)?;
        Ok(Candidate {
            endpoint,
            key,
            trusted_since: u64::try_from(trusted_at.as_second()).ok(),
        })
    }
}

fn entry_index(index: &str) -> Option<usize> {
    if index.is_empty()
        || index.len() > 5
        || !index.bytes().all(|byte| byte.is_ascii_digit())
        || (index.len() > 1 && index.starts_with('0'))
    {
        return None;
    }
    index.parse().ok()
}

fn property_value(raw: Option<&str>) -> Result<Cow<'_, str>, Omission> {
    let raw = raw.ok_or(Omission::IncompleteRecord)?;
    if !raw.contains('\\') {
        let value = raw.trim_matches(|ch| ch <= ' ');
        return (!value.is_empty())
            .then_some(Cow::Borrowed(value))
            .ok_or(Omission::IncompleteRecord);
    }
    // The Writer overload emits literal UTF-8 and does not write continuations
    // or \u escapes in values. Decode the escape sequences emitted by saveConvert.
    let mut value = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            value.push(ch);
            continue;
        }
        value.push(match chars.next() {
            Some('t') => '\t',
            Some('n') => '\n',
            Some('r') => '\r',
            Some('f') => '\x0c',
            Some(ch @ ('\\' | ' ' | '=' | ':' | '!' | '#')) => ch,
            _ => return Err(Omission::MalformedRecord),
        });
    }
    let trimmed = value.trim_matches(|ch| ch <= ' ');
    if trimmed.is_empty() {
        return Err(Omission::IncompleteRecord);
    }
    Ok(Cow::Owned(trimmed.into()))
}

fn invalid_property() -> io::Error {
    invalid("Unknown or malformed property in Java SSH host key store.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("java_host_pins/fixtures/ssh-host-keys.properties");
    const IPV6_PROPERTY: &str = r"entry.0.host=2001\:0db8\:0000\:0000\:0000\:0000\:0000\:0001";
    const FINGERPRINT: &str = "SHA256:bfswSR3tbmkn6n+aycr+Fyqm74aKeY4c9kVSpjN3wK4";

    #[test]
    fn reads_java_writer_output_and_keeps_exact_ports_and_fingerprints() {
        let decoded = decode(FIXTURE).unwrap();
        assert_eq!(decoded.alias_conflicts, 0);
        assert_eq!(
            decoded.omissions,
            BTreeMap::from([(Omission::UnsupportedEndpoint, 1)])
        );
        let [ipv6, dns] = decoded.candidates.as_slice() else {
            panic!("expected both supported host pins");
        };
        assert_eq!(ipv6.endpoint.host(), "2001:db8::1");
        assert_eq!(ipv6.endpoint.port(), 2222);
        assert_eq!(ipv6.key.algorithm(), "ssh-ed25519");
        assert_eq!(ipv6.key.fingerprint(), FINGERPRINT);
        let since = "2026-11-13T10:20:30Z".parse::<Timestamp>().unwrap();
        assert_eq!(ipv6.trusted_since, u64::try_from(since.as_second()).ok());
        assert_eq!(dns.endpoint.host(), "example.test");
        assert_eq!(dns.endpoint.port(), 22);
    }

    #[test]
    fn quarantines_conflicting_aliases_and_deduplicates_the_same_key() {
        let source = std::str::from_utf8(FIXTURE).unwrap();
        let duplicate = source
            .replace("entry.2.host=café.example", r"entry.2.host=2001\:db8\:\:1")
            .replace("entry.2.port=22", "entry.2.port=2222");
        let decoded = decode(duplicate.as_bytes()).unwrap();
        assert_eq!(decoded.candidates.len(), 2);
        assert_eq!(decoded.alias_conflicts, 0);
        assert_eq!(decoded.omissions.get(&Omission::DuplicateAlias), Some(&1));

        let conflict = duplicate
            .replace("entry.1.host=example.test", r"entry.1.host=2001\:0db8\:\:1")
            .replace("entry.1.port=22", "entry.1.port=2222");
        let decoded = decode(conflict.as_bytes()).unwrap();
        assert!(decoded.candidates.is_empty());
        assert_eq!(decoded.alias_conflicts, 1);
        assert_eq!(decoded.omissions.get(&Omission::ConflictingAlias), Some(&3));
    }

    #[test]
    fn omits_unsupported_incomplete_and_inconsistent_records() {
        let source = std::str::from_utf8(FIXTURE).unwrap();
        let mixed = source
            .replace("entry.0.algorithm=ssh-ed25519", "entry.0.algorithm=ssh-dss")
            .replace(
                "entry.1.fingerprintSha256=SHA256",
                "entry.1.fingerprintSha256=OTHER",
            )
            .replace("entry.2.port=22", "# missing port");
        let decoded = decode(mixed.as_bytes()).unwrap();
        assert!(decoded.candidates.is_empty());
        assert_eq!(
            decoded.omissions,
            BTreeMap::from([
                (Omission::UnsupportedAlgorithm, 1),
                (Omission::InconsistentKey, 1),
                (Omission::IncompleteRecord, 1),
            ])
        );
        let malformed = source.replace(IPV6_PROPERTY, r"entry.0.host=invalid\");
        assert_eq!(
            decode(malformed.as_bytes())
                .unwrap()
                .omissions
                .get(&Omission::MalformedRecord),
            Some(&1)
        );
    }

    #[test]
    fn rejects_duplicate_unknown_and_uncounted_properties() {
        let source = std::str::from_utf8(FIXTURE).unwrap();
        for added in [
            "format=1",
            "entry.0.port=2222",
            "entry.0.unknown=x",
            "entry.3.host=extra.test",
        ] {
            let extended = format!("{source}\n{added}\n");
            assert!(decode(extended.as_bytes()).is_err());
        }
        assert!(decode(b"format=1\nentry.count=0\n").is_err());
        assert!(decode(b"format=1\nentry.count=100001\n").is_err());
    }

    #[test]
    fn canonical_candidates_use_the_native_review_and_keep_existing_pins() {
        use kortty_ssh::{TrustMode, TrustStore};

        let decoded = decode(FIXTURE).unwrap();
        let [first, second] = decoded.candidates.as_slice() else {
            panic!("expected two source keys");
        };
        let store =
            TrustStore::from_pins(vec![(second.endpoint.clone(), first.key.clone())]).unwrap();
        assert_eq!(store.mode(), TrustMode::Strict);
        let review = store
            .preview_import(decoded.as_known_hosts().unwrap())
            .unwrap();
        let report = review.report();
        assert_eq!(report.added.len(), 1);
        assert_eq!(report.added[0].endpoint, first.endpoint);
        assert_eq!(report.added[0].key, first.key);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].endpoint, second.endpoint);
        assert_eq!(report.conflicts[0].trusted, first.key);
        assert_eq!(report.conflicts[0].offered, second.key);
    }

    #[test]
    fn backup_conversion_retains_public_candidates_outside_the_restore_snapshot() {
        use super::super::archive::java::{Member, MemberKind, Members, Protection};
        use super::super::java_migration;
        use zeroize::Zeroizing;

        let (snapshot, report) = java_migration::convert(Members {
            protection: Protection::Aes256,
            members: BTreeMap::from([(
                "ssh-host-keys.properties".into(),
                Member {
                    kind: MemberKind::HostPins,
                    bytes: Zeroizing::new(FIXTURE.to_vec()),
                },
            )]),
            unsupported: Vec::new(),
        })
        .unwrap();
        assert!(snapshot.is_empty());
        assert!(report.host_pins.is_some());
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry == "SSH host keys available for separate review (2)")
        );
        assert!(
            report
                .omissions
                .iter()
                .any(|entry| entry == "Java SSH host key records omitted (1)")
        );
    }

    #[test]
    fn decodes_writer_escaping_without_treating_value_separators_as_properties() {
        assert_eq!(
            property_value(Some(r"A\=\:\#\!\\\ B")).unwrap(),
            r"A=:#!\ B"
        );
        assert_eq!(
            property_value(Some(r"\ ")).unwrap_err(),
            Omission::IncompleteRecord
        );
        assert_eq!(
            property_value(Some(r"host\u002eexample")).unwrap_err(),
            Omission::MalformedRecord
        );
    }
}
