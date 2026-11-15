//! Java backups contain OpenPGP references without the certificates they identify.

use std::collections::HashSet;
use std::io;

use roxmltree::Node;
use zeroize::Zeroizing;

use crate::profile_backup::java_secrets::parse_xml;
use crate::snippets::recipients::{self, JavaRecipient, MAX_PENDING_JAVA};

use super::{Report, Snapshot, archive};

const MAX_XML_BYTES: usize = 1024 * 1024;

pub(super) fn convert(
    bytes: &[u8],
    snapshot: &mut Snapshot,
    report: &mut Report,
) -> io::Result<()> {
    let document = parse_xml(bytes, MAX_XML_BYTES, "gpgKeys")?;
    let mut pending = Vec::new();
    let mut ids = HashSet::new();
    let mut invalid = 0;
    let mut duplicate = 0;
    let mut full = 0;
    for node in document.root_element().children().filter(Node::is_element) {
        let Some((id, entry)) = read_key(node) else {
            invalid += 1;
            continue;
        };
        if ids.contains(id)
            || pending
                .iter()
                .any(|saved: &JavaRecipient| saved.fingerprint() == entry.fingerprint())
        {
            duplicate += 1;
        } else if pending.len() == MAX_PENDING_JAVA {
            full += 1;
        } else {
            ids.insert(id);
            pending.push(entry);
        }
    }
    report.note_count(
        invalid,
        "Java OpenPGP references with invalid or missing full fingerprints or metadata omitted",
    );
    report.note_count(duplicate, "Duplicate Java OpenPGP references omitted");
    report.note_count(
        full,
        "Java OpenPGP references above the import limit omitted",
    );
    if !pending.is_empty() {
        let count = pending.len();
        let delta = recipients::java_backup_delta(pending)?;
        if archive::remaining_capacity(snapshot)?(recipients::FILE_NAME, delta.len()) {
            snapshot.insert(recipients::FILE_NAME.into(), Zeroizing::new(delta));
            report.entries.push(format!(
                "OpenPGP recipient references requiring a public certificate and fingerprint verification ({count})"
            ));
            report.omissions.push(
                "Java archives do not include the OpenPGP certificates. Saved key paths and keyring references were omitted."
                    .into(),
            );
        } else {
            report.note_count(
                count,
                "Java OpenPGP references exceeding backup capacity omitted",
            );
        }
    }
    Ok(())
}

fn read_key<'a>(node: Node<'a, '_>) -> Option<(&'a str, JavaRecipient)> {
    if node.tag_name().namespace().is_some() || !node.has_tag_name("key") {
        return None;
    }
    let id = field(node, "id").ok()??;
    let name = field(node, "name").ok()??;
    let key_id = field(node, "keyId").ok()??;
    // Short or long legacy IDs may corroborate the full fingerprint. They
    // never identify a certificate by themselves.
    let key_id = key_id
        .strip_prefix("0x")
        .or_else(|| key_id.strip_prefix("0X"))
        .unwrap_or(key_id);
    let fingerprint = match field(node, "fingerprint").ok()? {
        Some(value) => value,
        None if matches!(key_id.len(), 40 | 64) => key_id,
        None => return None,
    };
    let entry = JavaRecipient::new(id, name, fingerprint).ok()?;
    let matches_id = match key_id.len() {
        8 | 16 => entry
            .key_id()
            .get(entry.key_id().len().checked_sub(key_id.len())?..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(key_id)),
        40 | 64 => entry.fingerprint().eq_ignore_ascii_case(key_id),
        _ => false,
    };
    matches_id.then_some((id, entry))
}

fn field<'a>(node: Node<'a, '_>, name: &str) -> Result<Option<&'a str>, ()> {
    let mut found = node.children().filter(|child| {
        child.is_element() && child.tag_name().namespace().is_none() && child.has_tag_name(name)
    });
    let Some(field) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() || field.children().any(|child| child.is_element()) {
        return Err(());
    }
    let mut values = field.children().filter(Node::is_text);
    let value = values.next().and_then(|node| node.text());
    if values.next().is_some() {
        return Err(());
    }
    Ok(value.map(str::trim).filter(|text| !text.is_empty()))
}
