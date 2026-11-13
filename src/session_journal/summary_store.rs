//! Field-scoped, restart-safe publication of a stopped journal's summary pass.

use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use roxmltree::{Document, Node};

use crate::private_files::Directory;

use super::entry_edit::{
    append_children, apply_patches, entries, lock_source, patch_scalars, require_java_fields,
};
use super::metadata::{MetadataLock, escape, lock_existing_metadata_in};
use super::summary_ai::{Card, ObservedUsage, State, Summary};
use super::summary_plan::{End, Window};
use super::viewer::Source;
use super::{
    DOCUMENT, Journal, boolean, child, document_id, field, invalid, journal, number, parse_document,
};

const GENERATION: &str = "nativeSummaryGeneration";
const CURSOR: &str = "nativeSummaryCursor";
const FINISHED: &str = "nativeSummaryFinished";
const RAW: &str = "nativeSummaryRawFallback";
const LAST_AI: &str = "lastSummarizedSeq";
pub(super) const MANUAL_TITLE: &str = "titleManuallySet";
const MAX_CARDS: usize = 512;
const AI_CARD: &str = "AI_SUMMARY";
const WRAP_UP: &str = "SESSION_SUMMARY";
const PROMPT_TOKENS: &str = "aiPromptTokens";
const COMPLETION_TOKENS: &str = "aiCompletionTokens";
const TOTAL_TOKENS: &str = "aiTotalTokens";
const CALL_COUNT: &str = "aiCallCount";
const USAGE_PARTIAL: &str = "aiUsagePartial";
// Native writers put the end time and source kind near the beginning of the document. If a
// capture's metadata exceeds the prefix, the ordinary bounded XML parser remains authoritative.
const RECOVERY_PREFIX_BYTES: u64 = 8 * 1024;

pub(super) struct TokenUsage {
    pub(super) prompt_tokens: u64,
    pub(super) completion_tokens: u64,
    pub(super) total_tokens: u64,
    pub(super) call_count: u64,
    pub(super) partial: bool,
}

impl TokenUsage {
    pub(super) fn read(meta: Node<'_, '_>) -> io::Result<Self> {
        let prompt_tokens = number(meta, PROMPT_TOKENS)?;
        let completion_tokens = number(meta, COMPLETION_TOKENS)?;
        let call_count = number(meta, CALL_COUNT)?.unwrap_or_default();
        Ok(Self {
            prompt_tokens: prompt_tokens.unwrap_or_default(),
            completion_tokens: completion_tokens.unwrap_or_default(),
            total_tokens: number(meta, TOTAL_TOKENS)?.unwrap_or_default(),
            call_count,
            partial: boolean(meta, USAGE_PARTIAL)?
                || call_count > 0 && (prompt_tokens.is_none() || completion_tokens.is_none()),
        })
    }

    fn observe(&mut self, observed: ObservedUsage) {
        fn add(value: &mut u64, delta: Option<u64>, limit: u64) -> bool {
            let Some(next) = delta.and_then(|delta| value.checked_add(delta)) else {
                return false;
            };
            if next > limit {
                return false;
            }
            *value = next;
            true
        }

        // Keep all persisted fields representable by the Java journal reader. A rejected value
        // leaves the last exact subtotal in place and makes the observation unusable for estimates.
        let limit = i64::MAX as u64;
        let total = observed
            .input_tokens
            .unwrap_or_default()
            .checked_add(observed.output_tokens.unwrap_or_default());
        self.partial |= !add(&mut self.prompt_tokens, observed.input_tokens, limit);
        self.partial |= !add(&mut self.completion_tokens, observed.output_tokens, limit);
        self.partial |= !add(&mut self.total_tokens, total, limit);
        self.partial |= !add(&mut self.call_count, Some(observed.calls), i32::MAX as u64);
        self.partial |= observed.partial;
    }

    fn fields(self) -> [(&'static str, String); 5] {
        [
            (PROMPT_TOKENS, self.prompt_tokens.to_string()),
            (COMPLETION_TOKENS, self.completion_tokens.to_string()),
            (TOTAL_TOKENS, self.total_tokens.to_string()),
            (CALL_COUNT, self.call_count.to_string()),
            (USAGE_PARTIAL, self.partial.to_string()),
        ]
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Progress {
    pub(crate) generation: Option<String>,
    pub(crate) cursor: u64,
    pub(crate) last_ai: u64,
    pub(crate) finished: bool,
    pub(crate) raw: bool,
}

impl Progress {
    pub(crate) fn read(source: &Source) -> io::Result<Self> {
        source.with_document(|_, document| progress(meta(document)?))
    }
}

pub(super) fn needs_summary(meta: Node<'_, '_>) -> bool {
    progress(meta).is_ok_and(|progress| {
        progress.raw || progress.last_ai == 0 || progress.generation.is_some() && !progress.finished
    })
}

pub(super) fn is_native_source(directory: &Path, id: &str) -> bool {
    native_id(id)
        && directory
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|name| name.strip_prefix("journal-"))
            == Some(id)
}

fn native_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) enum Update<'a> {
    /// Start a reviewed new pass after the selected AI profile passed preflight. Existing cards
    /// remain visible until the new pass has evidence for their ranges.
    Start {
        generation: &'a str,
        replay_from: Option<u64>,
    },
    Window {
        window: &'a Window,
        card: Option<&'a Card>,
        usage: Option<ObservedUsage>,
    },
    Finish {
        closing: &'a Summary,
        at: Timestamp,
        reviewed_title: Option<&'a str>,
        model: Option<&'a str>,
        usage: Option<ObservedUsage>,
    },
}

/// Applies an optimistic progress transition under the existing writer/delete lock. The selected
/// physical directory and document ID are both rechecked before publishing; raw logs are read-only.
pub(crate) fn apply(
    source: &Source,
    expected: &Progress,
    update: Update<'_>,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<(Progress, Journal)> {
    require_active(cancelled)?;
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The selected journal changed during summarization"));
    }
    let (next, output) = patch(&xml, &document, expected, update)?;
    let updated = parse_document(&output)?;
    let snapshot = journal(source.directory(), &updated)?;
    lock.publish_document(&stamp, Some(&output), |opened| {
        super::summary_ai::authorize_publication(authorize)?;
        require_active(cancelled)?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()?;
    Ok((next, snapshot))
}

/// Recovers interactive captures in the configured root without starting an AI request.
pub(crate) fn recover_interactive(root: &Path, cancelled: &AtomicBool) -> io::Result<usize> {
    require_active(cancelled)?;
    let opened_root = match Directory::open(root, false) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let root_identity = opened_root.identity()?;
    let mut recovered = 0;
    // Enumeration supplies names only. Every open and mutation is bound to the original root
    // handle and then to the source identity before publication.
    for entry in std::fs::read_dir(root)? {
        require_active(cancelled)?;
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some((name, id)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("journal-").map(|id| (name, id)))
        else {
            continue;
        };
        if !native_id(id) {
            continue;
        }
        let path = root.join(name);
        let result = (|| {
            let opened = opened_root.private_child(name, false)?;
            if !may_need_interactive_recovery(&opened, id)? {
                return Ok(false);
            }
            let source = Source::from_open_directory(&path, id, &opened)?;
            if !source.belongs_to_root(root_identity) {
                return Err(invalid("The configured session journal root changed"));
            }
            let lock = lock_existing_metadata_in(&path, opened)?;
            finish_abandoned_locked(&source, &lock, cancelled, None, true)
        })();
        if result.is_ok_and(|changed| changed) {
            recovered += 1;
        }
    }
    require_active(cancelled)?;
    Ok(recovered)
}

fn may_need_interactive_recovery(directory: &Directory, id: &str) -> io::Result<bool> {
    let file = directory.open_private_file(DOCUMENT)?;
    if file.metadata()?.permissions().readonly() {
        return Ok(false);
    }
    let mut prefix = Vec::with_capacity(RECOVERY_PREFIX_BYTES as usize);
    file.take(RECOVERY_PREFIX_BYTES).read_to_end(&mut prefix)?;
    let Some(end) = prefix
        .windows(b"</meta>".len())
        .position(|bytes| bytes == b"</meta>")
    else {
        return Ok(true);
    };
    prefix.truncate(end + b"</meta>".len());
    prefix.extend_from_slice(b"</session-journal>");
    let metadata = std::str::from_utf8(&prefix)
        .map_err(|_| invalid("Session journal metadata is not UTF-8"))
        .and_then(parse_document)
        .and_then(|document| {
            Ok(document_id(&document)? == id && interactive_open(meta(&document)?)?)
        });
    // An unusual or ambiguous prefix goes through the regular parser under the writer lock.
    Ok(metadata.unwrap_or(true))
}

fn interactive_open(meta: Node<'_, '_>) -> io::Result<bool> {
    Ok(number::<Timestamp>(meta, "endedAt")?.is_none() && field(meta, "sourceKind")?.is_none())
}

/// Finalizes a native capture left open by a previous process. The writer lock must be free
/// before the document can change.
pub(crate) fn finish_abandoned(source: &Source, cancelled: &AtomicBool) -> io::Result<()> {
    finish_abandoned_authorized(source, cancelled, None)
}

pub(crate) fn finish_abandoned_authorized(
    source: &Source,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<()> {
    require_active(cancelled)?;
    if !is_native_source(source.directory(), source.id()) {
        return Err(invalid("Expected an app-owned native session journal"));
    }
    let lock = lock_source(source)?;
    finish_abandoned_locked(source, &lock, cancelled, authorize, false).map(|_| ())
}

fn finish_abandoned_locked(
    source: &Source,
    lock: &MetadataLock,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
    interactive: bool,
) -> io::Result<bool> {
    require_active(cancelled)?;
    let (stamp, xml) = if interactive {
        let Some(document) = lock.read_writable_private_document()? else {
            return Ok(false);
        };
        document
    } else {
        lock.read_document()?
    };
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The abandoned session journal changed"));
    }
    let meta = meta(&document)?;
    let open = if interactive {
        interactive_open(meta)?
    } else {
        number::<Timestamp>(meta, "endedAt")?.is_none()
    };
    if !open {
        return Ok(false);
    }
    let mut ended = number::<Timestamp>(meta, "startedAt")?
        .ok_or_else(|| invalid("Missing journal start time"))?;
    let (latest, commands) = super::command_count::read(source, cancelled)?;
    if let Some(latest) = latest {
        ended = ended.max(latest);
    }
    let ended = ended.to_string();
    let commands = commands.map(|count| count.to_string());
    let mut patches = Vec::new();
    if let Some(commands) = &commands {
        patch_scalars(
            &xml,
            meta,
            [
                ("endedAt", Some(ended.as_str())),
                ("commandCount", Some(commands.as_str())),
            ],
            &mut patches,
        )?;
    } else {
        patch_scalars(
            &xml,
            meta,
            [("endedAt", Some(ended.as_str()))],
            &mut patches,
        )?;
    }
    let updated = apply_patches(&xml, patches)?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        super::summary_ai::authorize_publication(authorize)?;
        require_active(cancelled)?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()?;
    Ok(true)
}

pub(super) fn refresh_command_count(
    source: &Source,
    cancelled: &AtomicBool,
    authorize: Option<&super::summary_ai::Authorization>,
) -> io::Result<()> {
    require_active(cancelled)?;
    let lock = lock_source(source)?;
    let (stamp, xml) = lock.read_document()?;
    let document = parse_document(&xml)?;
    if document_id(&document)? != source.id() {
        return Err(invalid("The closed session journal changed"));
    }
    let meta = meta(&document)?;
    if number::<Timestamp>(meta, "endedAt")?.is_none() {
        return Err(invalid(
            "Finish the journal before classifying its commands",
        ));
    }
    let (_, Some(count)) = super::command_count::read(source, cancelled)? else {
        return Ok(());
    };
    if number::<u64>(meta, "commandCount")? == Some(count) {
        return Ok(());
    }
    let count = count.to_string();
    let mut patches = Vec::new();
    patch_scalars(
        &xml,
        meta,
        [("commandCount", Some(count.as_str()))],
        &mut patches,
    )?;
    let updated = apply_patches(&xml, patches)?;
    lock.publish_document(&stamp, Some(&updated), |opened| {
        super::summary_ai::authorize_publication(authorize)?;
        require_active(cancelled)?;
        source.require_location(source.directory(), opened)
    })?;
    lock.directory().sync()
}

fn require_active(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal summary cancelled",
        ))
    } else {
        Ok(())
    }
}

fn meta<'a, 'input>(document: &'a Document<'input>) -> io::Result<Node<'a, 'input>> {
    child(document.root_element(), "meta")
        .ok_or_else(|| invalid("Missing session journal metadata"))
}

fn progress(meta: Node<'_, '_>) -> io::Result<Progress> {
    let generation = field(meta, GENERATION)?.map(|value| value.into_owned());
    if generation
        .as_deref()
        .is_some_and(|id| !valid_generation(id))
    {
        return Err(invalid("Invalid session journal summary generation"));
    }
    let last_ai = number(meta, LAST_AI)?.unwrap_or_default();
    let cursor = number(meta, CURSOR)?.unwrap_or(last_ai);
    if last_ai > cursor {
        return Err(invalid("Invalid session journal summary checkpoint"));
    }
    Ok(Progress {
        generation,
        cursor,
        last_ai,
        finished: boolean(meta, FINISHED)?,
        raw: boolean(meta, RAW)?,
    })
}

fn patch(
    xml: &str,
    document: &Document<'_>,
    expected: &Progress,
    update: Update<'_>,
) -> io::Result<(Progress, String)> {
    let meta = meta(document)?;
    if number::<Timestamp>(meta, "endedAt")?.is_none() || progress(meta)? != *expected {
        return Err(invalid("The journal is recording or its summary changed"));
    }
    let wrapper = child(document.root_element(), "entries")
        .ok_or_else(|| invalid("Missing session journal entries"))?;
    require_java_fields(wrapper, &[])?;
    let mut patches = Vec::new();
    let mut next = expected.clone();
    let mut added = String::new();
    let mut metadata = Vec::new();
    let observed = match update {
        Update::Start {
            generation,
            replay_from,
        } => {
            if !valid_generation(generation) {
                return Err(invalid("Invalid session journal summary generation"));
            }
            next.generation = Some(generation.to_owned());
            next.finished = false;
            if let Some(checkpoint) = replay_from {
                if checkpoint > next.last_ai {
                    return Err(invalid("Invalid journal summary replay checkpoint"));
                }
                next.cursor = checkpoint;
                next.last_ai = checkpoint;
                next.raw = false;
            }
            None
        }
        Update::Window {
            window,
            card,
            usage,
        } => {
            let last = window
                .checkpoint()
                .ok_or_else(|| invalid("A closed journal cannot publish a running preview"))?;
            if next.generation.is_none()
                || next.finished
                || window.first <= next.cursor
                || last < window.first
                || window.skipped != card.is_none()
            {
                return Err(invalid("Invalid session journal summary range"));
            }
            patch_window(xml, document, window, card, &mut patches, &mut added)?;
            next.cursor = last;
            next.raw |= card.is_some_and(|card| card.state == State::Raw);
            if !next.raw {
                next.last_ai = last;
            }
            usage
        }
        Update::Finish {
            closing,
            at,
            reviewed_title,
            model,
            usage,
        } => {
            if next.generation.is_none() || next.finished {
                return Err(invalid("There is no active closing summary pass"));
            }
            let mut reused = false;
            for entry in entries(document) {
                if field(entry, "kind")?.as_deref() != Some(WRAP_UP) {
                    continue;
                }
                if !reused {
                    patch_scalars(
                        xml,
                        entry,
                        [
                            ("title", Some(closing.title.as_str())),
                            ("text", Some(closing.text.as_str())),
                        ],
                        &mut patches,
                    )?;
                    reused = true;
                } else {
                    preserve_or_remove(xml, entry, &mut patches)?;
                }
            }
            if !reused {
                new_entry(&mut added, WRAP_UP, at, &closing.title, &closing.text)?;
                added.push_str("</entry>");
            }
            if reviewed_title.is_some_and(|reviewed| {
                field(meta, "title").ok().flatten().as_deref() == Some(reviewed)
            }) && !boolean(meta, MANUAL_TITLE)?
            {
                metadata.push(("title", Some(closing.title.as_str())));
            }
            patch_list(
                xml,
                meta,
                "aiKeywords",
                "keyword",
                &closing.keywords,
                &mut patches,
            )?;
            if let Some(model) = model {
                metadata.push(("aiProfileName", Some(model)));
                metadata.push(("aiProfileLocal", Some("true")));
            }
            next.finished = true;
            usage
        }
    };
    let usage_fields = if let Some(observed) = observed {
        let mut usage = TokenUsage::read(meta)?;
        usage.observe(observed);
        Some(usage.fields())
    } else {
        None
    };
    if let Some(fields) = &usage_fields {
        metadata.extend(
            fields
                .iter()
                .map(|(name, value)| (*name, Some(value.as_str()))),
        );
    }
    let cursor = next.cursor.to_string();
    let last_ai = next.last_ai.to_string();
    metadata.extend([
        (GENERATION, next.generation.as_deref()),
        (CURSOR, Some(cursor.as_str())),
        (LAST_AI, Some(last_ai.as_str())),
        (FINISHED, Some(if next.finished { "true" } else { "false" })),
        (RAW, Some(if next.raw { "true" } else { "false" })),
    ]);
    patch_scalars(xml, meta, metadata, &mut patches)?;
    append_children(xml, wrapper, added, &mut patches)?;
    Ok((next, apply_patches(xml, patches)?))
}

type Patches = Vec<(Range<usize>, String)>;

fn patch_window(
    xml: &str,
    document: &Document<'_>,
    window: &Window,
    card: Option<&Card>,
    patches: &mut Patches,
    added: &mut String,
) -> io::Result<()> {
    let first = window.first.to_string();
    let last = window.last.to_string();
    let command_state = match window.end {
        End::Complete => None,
        End::Running => Some("RUNNING"),
        End::SessionEnded => Some("ENDED_WITH_SESSION"),
    };
    let generated = card.map(|card| {
        let state = match card.state {
            State::Raw => "RAW",
            State::Summarized => "SUMMARIZED",
        };
        [
            ("title", Some(card.summary.title.as_str())),
            ("text", Some(card.summary.text.as_str())),
            ("state", Some(state)),
            ("commandState", command_state),
            ("logStartSeq", Some(first.as_str())),
            ("logEndSeq", Some(last.as_str())),
        ]
    });
    let mut count = 0;
    let mut reused = false;
    for entry in entries(document) {
        if field(entry, "kind")?.as_deref() != Some(AI_CARD) {
            continue;
        }
        let start = number::<u64>(entry, "logStartSeq")?;
        let end = number::<u64>(entry, "logEndSeq")?;
        let overlaps = start
            .zip(end)
            .is_some_and(|(start, end)| start <= window.last && end >= window.first);
        if !overlaps {
            count += 1;
            continue;
        }
        if !reused
            && start == Some(window.first)
            && let Some((card, generated)) = card.zip(generated)
        {
            patch_scalars(xml, entry, generated, patches)?;
            patch_list(xml, entry, "inputExcerpt", "line", &card.input, patches)?;
            patch_list(xml, entry, "outputExcerpt", "line", &card.output, patches)?;
            reused = true;
        } else {
            preserve_or_remove(xml, entry, patches)?;
        }
    }
    if !reused && let Some((card, generated)) = card.zip(generated) {
        if count >= MAX_CARDS {
            return Err(invalid("The journal reached its summary card limit"));
        }
        new_entry(added, AI_CARD, window.started, "", "")?;
        for (name, text) in generated {
            if let Some(text) = text {
                element(added, name, text);
            }
        }
        list(added, "inputExcerpt", "line", &card.input);
        list(added, "outputExcerpt", "line", &card.output);
        added.push_str("</entry>");
    }
    Ok(())
}

fn preserve_or_remove(xml: &str, entry: Node<'_, '_>, patches: &mut Patches) -> io::Result<()> {
    let annotated = field(entry, "userNote")?.is_some_and(|note| !note.trim().is_empty())
        || matches!(
            field(entry, "markerSource")?.as_deref(),
            Some("MANUAL" | "USER")
        )
        || field(entry, "editedAt")?.is_some();
    if annotated {
        patch_scalars(xml, entry, [("kind", Some("USER_NOTE"))], patches)
    } else {
        patches.push((entry.range(), String::new()));
        Ok(())
    }
}

fn new_entry(
    xml: &mut String,
    kind: &str,
    at: Timestamp,
    title: &str,
    body: &str,
) -> io::Result<()> {
    xml.push_str("<entry>");
    element(xml, "id", &crate::private_files::nonce()?);
    element(xml, "kind", kind);
    element(xml, "createdAt", &at.to_string());
    if !title.is_empty() {
        element(xml, "title", title);
    }
    if !body.is_empty() {
        element(xml, "text", body);
    }
    Ok(())
}

fn patch_list(
    xml: &str,
    parent: Node<'_, '_>,
    wrapper: &'static str,
    item: &'static str,
    values: &[String],
    patches: &mut Patches,
) -> io::Result<()> {
    require_java_fields(parent, &[wrapper])?;
    let mut found = parent.children().filter(|node| node.has_tag_name(wrapper));
    let original = found.next();
    if found.next().is_some() {
        return Err(invalid("Duplicate journal summary list"));
    }
    let mut updated = String::new();
    list(&mut updated, wrapper, item, values);
    if let Some(original) = original {
        patches.push((original.range(), updated));
        Ok(())
    } else {
        append_children(xml, parent, updated, patches)
    }
}

fn list(xml: &mut String, wrapper: &str, item: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let _ = write!(xml, "<{wrapper}>");
    for value in values {
        element(xml, item, value);
    }
    let _ = write!(xml, "</{wrapper}>");
}

fn element(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, "<{name}>{}</{name}>", escape(value));
}

fn valid_generation(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_journal::{CaptureKind, CaptureRecord, summary_plan::Planner};

    #[test]
    fn journal_summary_range_replacement_keeps_the_card_identity_and_manual_fields() {
        let xml = r#"<session-journal formatVersion="1" id="fixture"><meta>
          <title>Handwritten title</title><titleManuallySet>true</titleManuallySet>
          <endedAt>2026-01-01T01:00:00Z</endedAt><nativeSummaryGeneration>0123456789abcdef0123456789abcdef</nativeSummaryGeneration>
          </meta><entries><entry><id>card</id><kind>AI_SUMMARY</kind><createdAt>2026-01-01T00:00:00Z</createdAt>
          <title>Previous</title><text>Old model output</text><logStartSeq>1</logStartSeq><logEndSeq>1</logEndSeq>
          <userNote>Keep this annotation</userNote><marker>ERROR</marker><markerSource>MANUAL</markerSource></entry>
          <entry><id>note</id><kind>USER_NOTE</kind><text>Unrelated note</text></entry></entries></session-journal>"#;
        let document = parse_document(xml).unwrap();
        let expected = progress(meta(&document).unwrap()).unwrap();
        let mut planner = Planner::resume(0);
        let time = Timestamp::from_second(1).unwrap();
        let _ = planner.push(CaptureRecord {
            sequence: 1,
            timestamp: time,
            kind: CaptureKind::Output,
            text: "New output".into(),
            redacted: false,
            partial: false,
            repeat: 1,
        });
        let window = planner.finish_pass(true, true, time).pop().unwrap();
        let card = Card::raw(&window);
        let (next, updated) = patch(
            xml,
            &document,
            &expected,
            Update::Window {
                window: &window,
                card: Some(&card),
                usage: None,
            },
        )
        .unwrap();
        let new = parse_document(&updated).unwrap();
        assert_eq!(next.cursor, 1);
        let rows: Vec<_> = entries(&new).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(field(rows[0], "id").unwrap().as_deref(), Some("card"));
        assert_eq!(
            field(rows[0], "userNote").unwrap().as_deref(),
            Some("Keep this annotation")
        );
        assert_eq!(
            field(rows[0], "markerSource").unwrap().as_deref(),
            Some("MANUAL")
        );
        assert_eq!(
            field(rows[1], "text").unwrap().as_deref(),
            Some("Unrelated note")
        );

        let closing = Summary {
            title: "Model title".into(),
            text: "New result".into(),
            keywords: vec!["result".into()],
        };
        let (_, final_xml) = patch(
            &updated,
            &new,
            &next,
            Update::Finish {
                closing: &closing,
                at: time,
                reviewed_title: Some("Handwritten title"),
                model: Some("local-fixture"),
                usage: None,
            },
        )
        .unwrap();
        let final_document = parse_document(&final_xml).unwrap();
        assert_eq!(
            field(meta(&final_document).unwrap(), "title")
                .unwrap()
                .as_deref(),
            Some("Handwritten title")
        );
        assert_eq!(
            entries(&final_document)
                .filter(|entry| field(*entry, "kind").unwrap().as_deref() == Some(WRAP_UP))
                .count(),
            1
        );
    }
}
