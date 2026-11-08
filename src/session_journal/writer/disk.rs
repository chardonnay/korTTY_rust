use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufWriter, Write as _};
use std::path::Path;
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use jiff::Timestamp;
use zeroize::Zeroizing;

use crate::private_files::Directory;

use super::super::annotation::{self, Annotation};
use super::super::capture::LiveArchives;
use super::super::entry_edit::delete::{Deletion, Outcome};
use super::super::entry_edit::{
    Changes, Edit, EntryState, Fields, LegacyMarker, MarkerOutcome, MarkerSource, MarkerState,
};
use super::super::markers::Compiled;
use super::super::screenshot_ai::Analysis;
use super::super::screenshot_analysis_store;
use super::super::screenshot_edit::{self, XmlWrite};
use super::super::viewer::Source;
use super::super::{
    LogFormat, MAX_DOCUMENT_BYTES, MAX_PNG_BYTES, MAX_PNG_PIXELS, MetadataEdit, display_text,
    invalid,
};
use super::storage::{DirectoryIdentity, MetadataError, Storage, file_identity};
use super::{Item, Metadata, PublishedScreenshot, RecordKind, Shared, uuid};

mod compression;
mod log_encoding;
mod markers;
mod screenshot_analysis;
use log_encoding::Record;
use markers::{Assignment, Snapshots, first_match};

const BATCH: usize = 64;
const MAX_TIMELINE: usize = 1024;
const MAX_TIMELINE_TEXT_BYTES: usize = 1024 * 1024;
const SCREENSHOTS: &str = "screenshots";
const LOG_LIMIT: &str = "the capture log reached its storage limit";
const TIMELINE_LIMIT: &str = "the journal timeline reached its storage limit";

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) part_bytes: u64,
    pub(super) parts: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            part_bytes: 25 * 1024 * 1024,
            parts: 20,
        }
    }
}

pub(super) struct Writer {
    directory: Storage,
    location: (DirectoryIdentity, DirectoryIdentity),
    screenshots: Option<Storage>,
    limits: Limits,
    log: Part,
    sealed_parts: Vec<SealedPart>,
    archives: Arc<LiveArchives>,
    document: Document,
    sequence: u64,
    timeline_text_bytes: usize,
    timeline_revision: usize,
}

impl Writer {
    pub(super) fn start(
        root: &Path,
        id: String,
        mut metadata: Metadata,
        limits: Limits,
        archives: Arc<LiveArchives>,
    ) -> io::Result<Self> {
        for value in [
            &mut metadata.title,
            &mut metadata.description,
            &mut metadata.connection_name,
            &mut metadata.host,
            &mut metadata.username,
        ] {
            if value.len() > 4096 {
                return Err(invalid("Session journal metadata exceeds the size limit"));
            }
            *value = display_text(value);
        }
        if metadata.title.trim().is_empty() {
            metadata.title = if metadata.connection_name.is_empty() {
                "Terminal".into()
            } else {
                metadata.connection_name.clone()
            };
        }
        let root = Storage::root(root)?;
        let root_identity = root.identity()?;
        let name = format!("journal-{id}");
        let mut directory = root.child(&name)?;
        let document = Document {
            id,
            metadata,
            pinned: false,
            started: Timestamp::now(),
            entries: Vec::new(),
            markers: Snapshots::default(),
            screenshot_count: 0,
        };
        let initialized = directory.lock_metadata().and_then(|()| {
            let directory_identity = directory.identity()?;
            let log = Part::new(&directory, &document, 1)?;
            document.save(&mut directory, 1, 0, None)?;
            Ok((log, directory_identity))
        });
        match initialized {
            Ok((log, directory_identity)) => Ok(Self {
                directory,
                location: (root_identity, directory_identity),
                screenshots: None,
                limits,
                log,
                sealed_parts: Vec::new(),
                archives,
                document,
                sequence: 0,
                timeline_text_bytes: 0,
                timeline_revision: 0,
            }),
            Err(error) => {
                directory.discard(&root, &name, &document.metadata.log_format.part_name(1));
                Err(error)
            }
        }
    }

    pub(super) fn location(&self) -> (DirectoryIdentity, DirectoryIdentity) {
        self.location
    }

    pub(super) fn run(
        mut self,
        input: &Receiver<Item>,
        shared: &Shared,
    ) -> io::Result<Option<&'static str>> {
        let truncation = self.record_until_closed(input, shared)?;
        if let Some(reason) = truncation {
            let time = Timestamp::now();
            let text = format!("Session journal capture stopped early: {reason}.");
            let mut entry = Entry::new("SYSTEM", time)?;
            entry.fields.text = Zeroizing::new(text);
            self.apply_new_marker(&mut entry, shared);
            self.document.entries.push(entry);
        }
        self.sealed_parts.push(self.log.finish()?);
        self.document.save(
            &mut self.directory,
            self.log.number,
            self.sequence,
            Some(Timestamp::now().max(self.document.started)),
        )?;
        if truncation.is_some() {
            self.advance_timeline();
        }
        self.publish(shared);
        drop(self.log);
        compression::archive(
            &self.directory,
            self.document.metadata.log_format,
            self.sealed_parts,
            &self.archives,
        );
        Ok(truncation)
    }

    fn record_until_closed(
        &mut self,
        input: &Receiver<Item>,
        shared: &Shared,
    ) -> io::Result<Option<&'static str>> {
        while let Ok(first) = input.recv_blocking() {
            let more = std::iter::from_fn(|| input.try_recv().ok());
            for item in std::iter::once(first).chain(more).take(BATCH) {
                if let Some(reason) = self.write(item, shared)? {
                    shared.finish(Some(reason));
                    return Ok(Some(reason));
                }
            }
            self.log.output.flush()?;
            self.publish(shared);
        }
        Ok(shared.lock().truncation)
    }

    fn publish(&self, shared: &Shared) {
        shared.publish(self.sequence, self.timeline_revision);
    }

    fn advance_timeline(&mut self) {
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    fn write(&mut self, item: Item, shared: &Shared) -> io::Result<Option<&'static str>> {
        match item {
            #[cfg(test)]
            Item::FailNextMetadataSync => self.directory.fail_next_metadata_sync.set(true),
            Item::Metadata { edit, reply } => {
                let result = self
                    .edit_metadata(edit)
                    .map_err(|error| format!("Could not save session journal metadata ({error})."));
                let _ = reply.try_send(result);
            }
            Item::Entry { edit, reply } => {
                let result = self.edit_entry(*edit, &shared.marker_rules()).map_err(|error| {
                    if error.published {
                        format!("The journal entry was saved, but its durability could not be confirmed ({}).", error.error)
                    } else {
                        format!("Could not save the session journal entry ({}).", error.error)
                    }
                });
                shared.lock().entry_mutation_pending = false;
                let _ = reply.try_send(result);
            }
            Item::DeleteEntry { deletion, reply } => {
                let result = self.delete_entry(&deletion).map_err(|error| {
                    format!("Could not delete the session journal entry ({error}).")
                });
                shared.lock().entry_mutation_pending = false;
                let _ = reply.try_send(result);
            }
            Item::EditScreenshot { edit, reply } => {
                let result = self
                    .edit_screenshot(&edit)
                    .map_err(|error| format!("Could not save the journal screenshot ({error})."));
                shared.lock().entry_mutation_pending = false;
                let _ = reply.try_send(result);
            }
            Item::ScreenshotAnalysis {
                asset,
                analysis,
                reply,
            } => {
                let result = self
                    .store_screenshot_analysis(&asset, analysis)
                    .map_err(|error| format!("Could not save the screenshot analysis ({error})."));
                shared.lock().entry_mutation_pending = false;
                let _ = reply.try_send(result);
            }
            Item::ApplyMarkers {
                source,
                rules,
                overwrite_manual,
                reply,
            } => {
                let result = self
                    .apply_marker_rules(&source, &rules, overwrite_manual)
                    .map_err(|error| {
                        format!("Could not apply session journal marker rules ({error}).")
                    });
                shared.lock().entry_mutation_pending = false;
                let _ = reply.try_send(result);
            }
            Item::Text {
                kind,
                text,
                partial,
                time,
            } => {
                let text = Zeroizing::new(display_text(&text));
                let timeline = if kind == RecordKind::Note && !text.trim().is_empty() {
                    if self.document.entries.len() >= MAX_TIMELINE
                        || self.timeline_text_bytes + text.len() > MAX_TIMELINE_TEXT_BYTES
                    {
                        return Ok(Some(TIMELINE_LIMIT));
                    }
                    Some(Entry::new("USER_NOTE", time)?)
                } else {
                    None
                };
                if !self.write_log(time, kind.key(), &text, partial, None)? {
                    return Ok(Some(LOG_LIMIT));
                }
                if let Some(mut entry) = timeline {
                    self.timeline_text_bytes += text.len();
                    entry.fields.text = text;
                    self.commit_entry(entry, shared)?;
                }
            }
            Item::Seed {
                lines,
                older_omitted,
                time,
            } => {
                let count = lines.len();
                for line in lines {
                    let line = Zeroizing::new(display_text(&line));
                    if !self.write_log(time, RecordKind::Seed.key(), &line, false, None)? {
                        return Ok(Some(LOG_LIMIT));
                    }
                }
                if count > 0 || older_omitted > 0 {
                    let mut text =
                        format!("journal enabled retroactively; {count} seed lines above");
                    if older_omitted > 0 {
                        let _ = write!(text, " ({older_omitted} older lines omitted)");
                    }
                    if !self.write_log(time, "note", &text, false, None)? {
                        return Ok(Some(LOG_LIMIT));
                    }
                }
            }
            Item::Screenshot { png, time, receipt } => {
                return self.save_screenshot(&png, time, receipt, shared);
            }
        }
        Ok(None)
    }

    fn save_screenshot(
        &mut self,
        png: &[u8],
        time: Timestamp,
        receipt: Option<Sender<Result<PublishedScreenshot, String>>>,
        shared: &Shared,
    ) -> io::Result<Option<&'static str>> {
        let result = self.write_screenshot(png, time, shared);
        shared.lock().screenshot_pending = false;
        if let Some(receipt) = receipt {
            let published = match &result {
                Ok(None) => {
                    self.publish(shared);
                    self.document
                        .entries
                        .last()
                        .and_then(|entry| {
                            Some(PublishedScreenshot {
                                id: entry.id.clone(),
                                reference: entry.screenshot.as_ref()?.clone(),
                            })
                        })
                        .ok_or_else(|| "The saved screenshot entry is unavailable.".to_owned())
                }
                Ok(Some(reason)) => {
                    Err(format!("Could not save the journal screenshot ({reason})."))
                }
                Err(error) => Err(format!("Could not save the journal screenshot ({error}).")),
            };
            let _ = receipt.try_send(published);
        }
        result
    }

    fn write_screenshot(
        &mut self,
        png: &[u8],
        time: Timestamp,
        shared: &Shared,
    ) -> io::Result<Option<&'static str>> {
        if self.document.entries.len() >= MAX_TIMELINE {
            return Ok(Some(TIMELINE_LIMIT));
        }
        let mut entry = Entry::new("SCREENSHOT", time)?;
        let filename = format!("shot-{:06}.png", self.sequence + 1);
        let reference = format!("{SCREENSHOTS}/{filename}");
        let bytes = self.encode_log(time, "screenshot", "", false, Some(&reference))?;
        if !self.reserve(bytes.len() as u64)? {
            return Ok(Some(LOG_LIMIT));
        }
        if self.screenshots.is_none() {
            self.screenshots = Some(self.directory.child(SCREENSHOTS)?);
        }
        self.screenshots
            .as_ref()
            .expect("screenshot directory prepared")
            .save_new(&filename, png)?;
        self.append_log(&bytes)?;
        entry.screenshot = Some(reference);
        self.document.screenshot_count += 1;
        self.commit_entry(entry, shared)?;
        Ok(None)
    }

    fn edit_metadata(&mut self, mut edit: MetadataEdit) -> io::Result<()> {
        if matches!(edit, MetadataEdit::Pinned(pinned) if pinned == self.document.pinned) {
            return self.directory.require_metadata();
        }
        self.log.sync()?;
        match &mut edit {
            MetadataEdit::Title(value) => {
                std::mem::swap(&mut self.document.metadata.title, value);
            }
            MetadataEdit::Description(value) => {
                std::mem::swap(&mut self.document.metadata.description, value);
            }
            MetadataEdit::Pinned(value) => {
                std::mem::swap(&mut self.document.pinned, value);
            }
        }
        let result = self
            .document
            .save(&mut self.directory, self.log.number, self.sequence, None);
        if result.as_ref().is_err_and(|error| !error.published) {
            match edit {
                MetadataEdit::Title(previous) => self.document.metadata.title = previous,
                MetadataEdit::Description(previous) => {
                    self.document.metadata.description = previous;
                }
                MetadataEdit::Pinned(previous) => self.document.pinned = previous,
            }
        }
        result.map_err(io::Error::from)
    }

    fn edit_entry(&mut self, edit: Edit, current_rules: &Compiled) -> Result<(), MetadataError> {
        let index = self.document.entry_index(edit.id())?;
        let entry = &self.document.entries[index];
        edit.require_current(&entry.edit_state())?;
        let source = edit.source().clone();
        let opened = Directory::open(source.directory(), false)?;
        source.require_location(source.directory(), &opened)?;
        self.directory.require_metadata()?;
        let (fields, marker) = match edit.into_changes() {
            Changes::Fields { fields, marker } => (Some(EntryFields::from(fields)), marker),
            Changes::Marker(marker) => (None, marker),
        };
        let definition = self
            .document
            .markers
            .manual_choice(&marker, current_rules)?;
        let text_bytes = fields.as_ref().map_or(self.timeline_text_bytes, |fields| {
            self.timeline_text_bytes - entry.fields.byte_len() + fields.byte_len()
        });
        if text_bytes > MAX_TIMELINE_TEXT_BYTES {
            return Err(invalid("Session journal timeline text exceeds its storage limit").into());
        }
        let snapshots = if definition.is_builtin() {
            None
        } else {
            let mut snapshots = self.document.markers.clone();
            snapshots.upsert(&definition)?;
            Some(snapshots)
        };
        self.log.sync()?;
        let old_snapshots =
            snapshots.map(|snapshots| std::mem::replace(&mut self.document.markers, snapshots));
        let entry = &mut self.document.entries[index];
        let old_fields = fields.map(|fields| std::mem::replace(&mut entry.fields, fields));
        let old_marker = entry
            .marker
            .replace(Assignment::new(&definition, MarkerSource::User));
        let old_edited_at = entry.edited_at.replace(Timestamp::now());
        let result = self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            || source.require_location(source.directory(), &opened),
        );
        if result.as_ref().is_err_and(|error| !error.published) {
            let entry = &mut self.document.entries[index];
            if let Some(fields) = old_fields {
                entry.fields = fields;
            }
            entry.marker = old_marker;
            entry.edited_at = old_edited_at;
            if let Some(snapshots) = old_snapshots {
                self.document.markers = snapshots;
            }
        } else {
            self.timeline_text_bytes = text_bytes;
            self.advance_timeline();
        }
        result
    }

    fn apply_marker_rules(
        &mut self,
        source: &Source,
        rules: &Compiled,
        overwrite_manual: bool,
    ) -> io::Result<MarkerOutcome> {
        let opened = Directory::open(source.directory(), false)?;
        source.require_location(source.directory(), &opened)?;
        self.directory.require_metadata()?;
        let mut snapshots = self.document.markers.clone();
        let mut updates = Vec::new();
        for (index, entry) in self.document.entries.iter().enumerate() {
            if !overwrite_manual
                && entry
                    .marker
                    .as_ref()
                    .is_some_and(|marker| marker.source == MarkerSource::User)
            {
                continue;
            }
            if let Some(definition) = first_match(rules, entry) {
                let marker = Assignment::new(definition, MarkerSource::Rule);
                if entry.marker.as_ref() != Some(&marker) {
                    snapshots.upsert(definition)?;
                    updates.push((index, marker));
                }
            }
        }
        let changed = updates.len();
        if changed == 0 {
            return Ok(MarkerOutcome::default());
        }
        self.log.sync()?;
        let old_snapshots = std::mem::replace(&mut self.document.markers, snapshots);
        let old_markers: Vec<_> = updates
            .into_iter()
            .map(|(index, marker)| (index, self.document.entries[index].marker.replace(marker)))
            .collect();
        let durability_warning = match self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            || source.require_location(source.directory(), &opened),
        ) {
            Ok(()) => false,
            Err(error) if error.published => true,
            Err(error) => {
                self.document.markers = old_snapshots;
                for (index, marker) in old_markers {
                    self.document.entries[index].marker = marker;
                }
                return Err(error.error);
            }
        };
        self.advance_timeline();
        Ok(MarkerOutcome {
            changed,
            durability_warning,
        })
    }

    fn delete_entry(&mut self, deletion: &Deletion) -> io::Result<Outcome> {
        let index = self.document.entry_index(deletion.id())?;
        let entry = &self.document.entries[index];
        {
            let mut row = Zeroizing::new(String::new());
            entry.write_xml(&mut row)?;
            deletion.require_current(&entry.edit_state(), entry.screenshot.as_deref(), &row)?;
        }

        let source = deletion.source();
        let opened = Directory::open(source.directory(), false)?;
        source.require_location(source.directory(), &opened)?;
        self.directory.require_metadata()?;
        let retained = self
            .document
            .entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                if position == index {
                    None
                } else {
                    entry.screenshot.as_deref()
                }
            });
        let cleanup = deletion.image_cleanup(&opened, retained);
        self.log.sync()?;

        let removed = self.document.entries.remove(index);
        let old_snapshots = self.document.markers.clone();
        self.document.markers.prune_unused(&self.document.entries);
        let durability_warning = match self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            || source.require_location(source.directory(), &opened),
        ) {
            Ok(()) => false,
            Err(error) if error.published => true,
            Err(error) => {
                self.document.entries.insert(index, removed);
                self.document.markers = old_snapshots;
                return Err(error.error);
            }
        };
        self.timeline_text_bytes -= removed.fields.byte_len();
        self.advance_timeline();

        let cleanup_warning = !durability_warning
            && cleanup
                .and_then(|plan| {
                    if let Some(plan) = plan {
                        plan.remove(&opened, || {
                            source.require_location(source.directory(), &opened)?;
                            self.directory.require_metadata()
                        })?;
                    }
                    Ok(())
                })
                .is_err();
        Ok(Outcome {
            cleanup_warning,
            durability_warning,
        })
    }

    fn edit_screenshot(
        &mut self,
        edit: &screenshot_edit::Edit,
    ) -> io::Result<screenshot_edit::Outcome> {
        let index = self.document.entry_index(edit.id())?;
        let entry = &self.document.entries[index];
        if entry.kind != "SCREENSHOT" || entry.screenshot.as_deref() != Some(edit.reference()) {
            return Err(invalid("The selected journal screenshot changed"));
        }
        edit.require_row(
            Some(&entry.time.to_string()),
            &entry.annotations,
            &entry.fields.note,
        )?;
        edit.require_exclusive(
            self.document
                .entries
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .filter_map(|(_, entry)| entry.screenshot.as_deref()),
        )?;
        let text_bytes = self.timeline_text_bytes - entry.fields.note.len() + edit.note().len();
        if text_bytes > MAX_TIMELINE_TEXT_BYTES {
            return Err(invalid(
                "Session journal timeline text exceeds its storage limit",
            ));
        }

        let source = edit.source();
        let opened = Directory::open(source.directory(), false)?;
        let require_location = || source.require_location(source.directory(), &opened);
        require_location()?;
        self.directory.require_metadata()?;
        if !edit.annotations_changed() && !edit.note_changed() {
            return Ok(screenshot_edit::Outcome::default());
        }
        self.log.sync()?;

        let entry = &mut self.document.entries[index];
        let previous_annotations =
            std::mem::replace(&mut entry.annotations, edit.annotations().to_vec());
        let previous_note = std::mem::replace(
            &mut entry.fields.note,
            Zeroizing::new(edit.note().to_owned()),
        );
        let previous_edited_at = entry.edited_at.replace(Timestamp::now());
        let mut published = false;
        let result = edit.publish_with(&opened, |operation| match operation {
            XmlWrite::Check => {
                require_location()?;
                self.directory.require_metadata()?;
                Ok(false)
            }
            XmlWrite::Commit(verify) => match self.document.save_checked(
                &mut self.directory,
                self.log.number,
                self.sequence,
                None,
                || {
                    require_location()?;
                    verify()
                },
            ) {
                Ok(()) => {
                    published = true;
                    Ok(false)
                }
                Err(error) if error.published => {
                    published = true;
                    Ok(true)
                }
                Err(error) => Err(error.error),
            },
        });
        if published {
            self.timeline_text_bytes = text_bytes;
            self.advance_timeline();
        } else {
            let entry = &mut self.document.entries[index];
            entry.annotations = previous_annotations;
            entry.fields.note = previous_note;
            entry.edited_at = previous_edited_at;
        }
        result
    }

    fn commit_entry(&mut self, mut entry: Entry, shared: &Shared) -> io::Result<()> {
        self.log.sync()?;
        entry.anchor = Some((self.log.number, self.sequence));
        self.apply_new_marker(&mut entry, shared);
        self.document.entries.push(entry);
        self.document
            .save(&mut self.directory, self.log.number, self.sequence, None)
            .map_err(io::Error::from)?;
        self.advance_timeline();
        Ok(())
    }

    fn apply_new_marker(&mut self, entry: &mut Entry, shared: &Shared) {
        let rules = shared.marker_rules();
        if rules.enabled()
            && let Some(definition) = first_match(&rules, entry)
            // A full optional snapshot registry must not stop capture.
            && self.document.markers.upsert(definition).is_ok()
        {
            entry.marker = Some(Assignment::new(definition, MarkerSource::Rule));
        }
    }

    fn write_log(
        &mut self,
        time: Timestamp,
        kind: &str,
        text: &str,
        partial: bool,
        file: Option<&str>,
    ) -> io::Result<bool> {
        let bytes = self.encode_log(time, kind, text, partial, file)?;
        if !self.reserve(bytes.len() as u64)? {
            return Ok(false);
        }
        self.append_log(&bytes)?;
        Ok(true)
    }

    fn encode_log(
        &self,
        time: Timestamp,
        kind: &str,
        text: &str,
        partial: bool,
        file: Option<&str>,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        Record {
            seq: self.sequence + 1,
            t: time.to_string(),
            k: kind,
            x: text,
            partial,
            file,
        }
        .encode(self.document.metadata.log_format)
    }

    fn reserve(&mut self, bytes: u64) -> io::Result<bool> {
        if self.log.bytes + bytes + self.log.footer.len() as u64 <= self.limits.part_bytes {
            return Ok(true);
        }
        if self.log.number >= self.limits.parts {
            return Ok(false);
        }
        let sealed = self.log.finish()?;
        self.log = Part::new(&self.directory, &self.document, self.log.number + 1)?;
        self.document
            .save(&mut self.directory, self.log.number, self.sequence, None)?;
        if !compression::rotate(
            &self.directory,
            self.document.metadata.log_format,
            &sealed,
            &self.archives,
        ) {
            self.sealed_parts.push(sealed);
        }
        Ok(self.log.bytes + bytes + self.log.footer.len() as u64 <= self.limits.part_bytes)
    }

    fn append_log(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.log.output.write_all(bytes)?;
        self.log.bytes += bytes.len() as u64;
        self.sequence += 1;
        Ok(())
    }
}

struct Part {
    number: u32,
    bytes: u64,
    output: BufWriter<File>,
    footer: &'static [u8],
    identity: DirectoryIdentity,
}

impl Part {
    fn new(directory: &Storage, document: &Document, number: u32) -> io::Result<Self> {
        let format = document.metadata.log_format;
        let header = log_encoding::header(format, document, number)?;
        let mut file = directory.file(&format.part_name(number))?;
        let identity = file_identity(&file)?;
        file.write_all(&header)?;
        file.sync_all()?;
        Ok(Self {
            number,
            bytes: header.len() as u64,
            output: BufWriter::new(file),
            footer: log_encoding::footer(format),
            identity,
        })
    }

    fn finish(&mut self) -> io::Result<SealedPart> {
        self.output.write_all(self.footer)?;
        self.sync()?;
        Ok(SealedPart {
            number: self.number,
            bytes: self.bytes + self.footer.len() as u64,
            identity: self.identity,
        })
    }

    fn sync(&mut self) -> io::Result<()> {
        self.output.flush()?;
        self.output.get_ref().sync_all()
    }
}

struct Document {
    id: String,
    metadata: Metadata,
    pinned: bool,
    started: Timestamp,
    entries: Vec<Entry>,
    markers: Snapshots,
    screenshot_count: u32,
}

struct Entry {
    id: String,
    kind: &'static str,
    time: Timestamp,
    fields: EntryFields,
    marker: Option<Assignment>,
    edited_at: Option<Timestamp>,
    screenshot: Option<String>,
    annotations: Vec<Annotation>,
    analysis: Option<Arc<Analysis>>,
    anchor: Option<(u32, u64)>,
}

impl Entry {
    fn new(kind: &'static str, time: Timestamp) -> io::Result<Self> {
        Ok(Self {
            id: uuid()?,
            kind,
            time,
            fields: EntryFields::default(),
            marker: None,
            edited_at: None,
            screenshot: None,
            annotations: Vec::new(),
            analysis: None,
            anchor: None,
        })
    }

    fn legacy_marker(&self) -> LegacyMarker {
        self.marker
            .as_ref()
            .map_or(LegacyMarker::None, |marker| marker.legacy)
    }

    fn edit_state(&self) -> EntryState {
        EntryState {
            fields: Fields {
                title: self.fields.title.as_str().to_owned(),
                text: self.fields.text.as_str().to_owned(),
                note: self.fields.note.as_str().to_owned(),
            },
            marker: MarkerState {
                legacy: Some(self.legacy_marker().xml().into()),
                id: self.marker.as_ref().and_then(|marker| marker.id.clone()),
                source: self
                    .marker
                    .as_ref()
                    .map(|marker| marker.source.xml().into()),
            },
        }
    }

    fn write_xml(&self, xml: &mut String) -> io::Result<()> {
        xml.push_str("<entry>\n");
        element(xml, "id", &self.id);
        element(xml, "kind", self.kind);
        element(xml, "marker", self.legacy_marker().xml());
        if let Some(marker) = &self.marker {
            if let Some(id) = &marker.id {
                element(xml, "markerId", id);
            }
            element(xml, "markerSource", marker.source.xml());
        }
        element(xml, "createdAt", &self.time.to_string());
        if let Some(edited_at) = self.edited_at {
            element(xml, "editedAt", &edited_at.to_string());
        }
        for (name, value) in [
            ("title", &self.fields.title),
            ("text", &self.fields.text),
            ("userNote", &self.fields.note),
        ] {
            if !value.is_empty() {
                element(xml, name, value);
            }
        }
        if let Some(screenshot) = &self.screenshot {
            element(xml, "screenshotFile", screenshot);
        }
        annotation::write_xml(xml, &self.annotations)?;
        if let Some(analysis) = &self.analysis {
            screenshot_analysis_store::write_xml(xml, analysis);
        }
        if let Some((part, seq)) = self.anchor {
            element(xml, "logPart", &part.to_string());
            element(xml, "logStartSeq", &seq.to_string());
            element(xml, "logEndSeq", &seq.to_string());
        }
        xml.push_str("</entry>");
        Ok(())
    }
}

#[derive(Default)]
struct EntryFields {
    title: Zeroizing<String>,
    text: Zeroizing<String>,
    note: Zeroizing<String>,
}

impl EntryFields {
    fn byte_len(&self) -> usize {
        self.title.len() + self.text.len() + self.note.len()
    }
}

impl From<Fields> for EntryFields {
    fn from(fields: Fields) -> Self {
        Self {
            title: Zeroizing::new(fields.title),
            text: Zeroizing::new(fields.text),
            note: Zeroizing::new(fields.note),
        }
    }
}

impl Document {
    fn entry_index(&self, id: &str) -> io::Result<usize> {
        self.entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| invalid("The selected journal entry no longer exists"))
    }

    fn save(
        &self,
        directory: &mut Storage,
        parts: u32,
        records: u64,
        ended: Option<Timestamp>,
    ) -> Result<(), MetadataError> {
        self.save_checked(directory, parts, records, ended, || Ok(()))
    }

    fn save_checked(
        &self,
        directory: &mut Storage,
        parts: u32,
        records: u64,
        ended: Option<Timestamp>,
        verify: impl Fn() -> io::Result<()>,
    ) -> Result<(), MetadataError> {
        let mut xml = Zeroizing::new(String::new());
        let _ = write!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<session-journal formatVersion=\"1\" id=\"{}\">\n<meta>\n",
            self.id
        );
        for (name, value) in [
            ("title", self.metadata.title.as_str()),
            ("description", &self.metadata.description),
            ("connectionName", &self.metadata.connection_name),
            ("host", &self.metadata.host),
            ("username", &self.metadata.username),
            ("appVersion", env!("CARGO_PKG_VERSION")),
        ] {
            element(&mut xml, name, value);
        }
        element(
            &mut xml,
            "port",
            &self.metadata.port.unwrap_or(0).to_string(),
        );
        element(&mut xml, "startedAt", &self.started.to_string());
        if let Some(ended) = ended {
            element(&mut xml, "endedAt", &ended.to_string());
        }
        element(
            &mut xml,
            "seeded",
            if self.metadata.seeded {
                "true"
            } else {
                "false"
            },
        );
        element(&mut xml, "logFormat", self.metadata.log_format.name());
        element(&mut xml, "logEntryCount", &records.to_string());
        element(&mut xml, "logParts", &parts.to_string());
        let errors = self
            .entries
            .iter()
            .filter(|entry| entry.legacy_marker() == LegacyMarker::Error)
            .count();
        element(&mut xml, "errorCount", &errors.to_string());
        element(
            &mut xml,
            "screenshotCount",
            &self.screenshot_count.to_string(),
        );
        element(
            &mut xml,
            "pinned",
            if self.pinned { "true" } else { "false" },
        );
        element(&mut xml, "aiSummariesOff", "true");
        xml.push_str("</meta>\n<entries>\n");
        for entry in &self.entries {
            entry.write_xml(&mut xml)?;
            xml.push('\n');
        }
        xml.push_str("</entries>\n");
        self.markers.write_xml(&mut xml);
        xml.push_str("</session-journal>\n");
        if xml.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(invalid("Session journal document exceeds its size limit").into());
        }
        directory.metadata_checked(xml.as_bytes(), verify)
    }
}

fn element(xml: &mut String, name: &str, value: &str) {
    let _ = write!(xml, "<{name}>");
    for character in value.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '\r' => xml.push_str("&#13;"),
            '\t'
            | '\n'
            | ' '..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}' => xml.push(character),
            _ => {}
        }
    }
    let _ = writeln!(xml, "</{name}>");
}

pub(super) fn valid_png(bytes: &[u8]) -> bool {
    if bytes.len() < 33
        || bytes.len() as u64 > MAX_PNG_BYTES
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")
    {
        return false;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    width != 0 && height != 0 && u64::from(width) * u64::from(height) <= MAX_PNG_PIXELS
}
use compression::SealedPart;
