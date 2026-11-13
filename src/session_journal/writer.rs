//! Private, bounded capture writer for interactive Java v1 journals.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;

use async_channel::{Receiver, Sender, TrySendError};
use jiff::Timestamp;
use zeroize::Zeroizing;

use super::{
    LiveCursor, LivePage, LogFormat, MetadataEdit, agent_chat, capture, entry_edit, markers,
    screenshot_ai, screenshot_analysis_store, screenshot_edit, viewer,
};
use entry_edit::MarkerOutcome;
use entry_edit::delete::{Deletion, Outcome};

mod disk;
mod storage;
pub(super) use storage::file_identity;

#[cfg(test)]
mod tests;

use disk::{Limits, Writer};

const QUEUE: usize = 256;
pub(crate) const MAX_RECORD_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SEED_LINES: usize = 8000;
pub(crate) const MAX_SEED_BYTES: usize = 1024 * 1024;
const MAX_SCREENSHOT_TOTAL: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct Metadata {
    pub title: String,
    pub description: String,
    pub connection_name: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub seeded: bool,
    pub log_format: LogFormat,
    pub(crate) connection_config: Option<super::ConnectionConfig>,
    pub(crate) title_manually_set: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Recording,
    Finishing,
    Complete,
    Truncated(String),
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PublishedScreenshot {
    pub(crate) id: String,
    pub(crate) reference: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecordKind {
    Seed,
    Output,
    Input,
    CommandEnd,
    Note,
    SystemNote,
}

impl RecordKind {
    fn key(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Output => "out",
            Self::Input => "in",
            Self::CommandEnd | Self::Note | Self::SystemNote => "note",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SendError {
    Closed,
    Full,
    TooLarge,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "The session journal is closing",
            Self::Full => "The session journal queue is full",
            Self::TooLarge => "The session journal item exceeds its size limit",
        })
    }
}

impl std::error::Error for SendError {}

pub(crate) struct Capture {
    id: String,
    started_at: Timestamp,
    directory: PathBuf,
    root_identity: storage::DirectoryIdentity,
    directory_identity: storage::DirectoryIdentity,
    log_format: LogFormat,
    summary_config: Option<super::ConnectionConfig>,
    archives: Arc<capture::LiveArchives>,
    sink: Sink,
    done: Receiver<()>,
}

impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionJournalCapture")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Opens private storage and waits for the on-disk header. Call off the UI thread.
    #[cfg(test)]
    pub(crate) fn start(root: PathBuf, metadata: Metadata) -> io::Result<Self> {
        Self::start_with_marker_rules(root, metadata, Arc::new(markers::Compiled::default()))
    }

    #[cfg(test)]
    pub(crate) fn start_with_marker_rules(
        root: PathBuf,
        metadata: Metadata,
        rules: Arc<markers::Compiled>,
    ) -> io::Result<Self> {
        Self::start_with_limits_and_rules(root, metadata, Limits::default(), rules, None)
    }

    pub(crate) fn start_configured(
        root: PathBuf,
        metadata: Metadata,
        rules: Arc<markers::Compiled>,
    ) -> io::Result<Self> {
        Self::start_configured_inner(root, metadata, rules, None)
    }

    pub(super) fn start_configured_automation(
        root: PathBuf,
        metadata: Metadata,
        rules: Arc<markers::Compiled>,
        automation: super::automation::TargetMetadata,
    ) -> io::Result<Self> {
        Self::start_configured_inner(root, metadata, rules, Some(automation))
    }

    fn start_configured_inner(
        root: PathBuf,
        metadata: Metadata,
        rules: Arc<markers::Compiled>,
        automation: Option<super::automation::TargetMetadata>,
    ) -> io::Result<Self> {
        let config = metadata.connection_config.unwrap_or_default();
        config.validate()?;
        let limits = Limits {
            part_bytes: config.part_bytes(),
            parts: u32::from(config.max_log_parts),
        };
        Self::start_with_limits_and_rules(root, metadata, limits, rules, automation)
    }

    #[cfg(test)]
    fn start_with_limits(root: PathBuf, metadata: Metadata, limits: Limits) -> io::Result<Self> {
        Self::start_with_limits_and_rules(
            root,
            metadata,
            limits,
            Arc::new(markers::Compiled::default()),
            None,
        )
    }

    fn start_with_limits_and_rules(
        root: PathBuf,
        metadata: Metadata,
        limits: Limits,
        rules: Arc<markers::Compiled>,
        automation: Option<super::automation::TargetMetadata>,
    ) -> io::Result<Self> {
        let started_at = Timestamp::now();
        let id = uuid()?;
        let directory = root.join(format!("journal-{id}"));
        let (send, receive) = async_channel::bounded(QUEUE);
        let shared = Arc::new(Shared {
            input: send,
            output_paused: AtomicBool::new(false),
            marker_rules: Mutex::new(rules),
            sequence: AtomicU64::default(),
            timeline: AtomicUsize::default(),
            progress: Mutex::new(Progress {
                status: Status::Recording,
                truncation: None,
                received_text: false,
                seed_sent: false,
                screenshot_pending: false,
                entry_mutation_pending: false,
                screenshot_bytes: 0,
            }),
        });
        let (completion, done) = async_channel::bounded(1);
        let (ready, started) = mpsc::sync_channel(1);
        let completion = Completion {
            shared: Arc::clone(&shared),
            notification: completion,
        };
        let writer_id = id.clone();
        let log_format = metadata.log_format;
        let summary_config = metadata.connection_config;
        let archives = Arc::new(capture::LiveArchives::default());
        let writer_archives = Arc::clone(&archives);
        thread::Builder::new()
            .name("kortty-session-journal".into())
            .spawn(move || {
                let writer = Writer::start(
                    &root,
                    writer_id,
                    metadata,
                    limits,
                    writer_archives,
                    automation,
                );
                match writer {
                    Ok(writer) => {
                        if ready.send(Ok(writer.location())).is_err() {
                            completion.shared.input.close();
                        }
                        let status = writer.run(&receive, &completion.shared);
                        completion.shared.input.close();
                        while receive.try_recv().is_ok() {}
                        completion.shared.lock().status = match status {
                            Ok(Some(reason)) => Status::Truncated(reason.into()),
                            Ok(None) => Status::Complete,
                            Err(error) => {
                                Status::Failed(format!("Could not save session journal ({error})."))
                            }
                        };
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })?;
        let (root_identity, directory_identity) = started
            .recv()
            .map_err(|_| io::Error::other("Session journal writer stopped during startup"))??;
        Ok(Self {
            id,
            started_at,
            directory,
            root_identity,
            directory_identity,
            log_format,
            summary_config,
            archives,
            sink: Sink { shared },
            done,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn started_at(&self) -> Timestamp {
        self.started_at
    }

    pub(crate) fn log_format(&self) -> LogFormat {
        self.log_format
    }

    pub(crate) fn summary_config(&self) -> Option<super::ConnectionConfig> {
        self.summary_config
    }

    pub(crate) fn publish_summary(
        &self,
        after: u64,
        window: super::summary_plan::Window,
        card: Option<super::summary_ai::Card>,
        cancelled: Arc<AtomicBool>,
        authorize: Option<super::summary_ai::Authorization>,
    ) -> io::Result<Receiver<Result<u64, String>>> {
        self.enqueue_mutation(move |reply| Item::Summary {
            update: Box::new(disk::SummaryPublish {
                after,
                window,
                card,
                cancelled,
                authorize,
            }),
            reply,
        })
    }

    pub(crate) fn live_tail(&self, after: Option<&LiveCursor>) -> io::Result<LivePage> {
        capture::capture_live_tail(self.directory(), self.log_format(), &self.archives, after)
    }

    /// Appends a masked Agent card once, in the same FIFO as capture and entry edits.
    pub(crate) fn ensure_agent_card(
        &self,
        card: agent_chat::Card,
    ) -> io::Result<Receiver<Result<(), String>>> {
        self.ensure_agent_card_checked(card, None)
    }

    pub(crate) fn ensure_agent_card_checked(
        &self,
        card: agent_chat::Card,
        permit: Option<Arc<AtomicBool>>,
    ) -> io::Result<Receiver<Result<(), String>>> {
        card.validate()?;
        self.enqueue_mutation(move |reply| Item::AgentCard {
            card: Box::new(card),
            permit,
            reply,
        })
    }

    #[cfg(test)]
    pub(crate) fn link_agent_chat(
        &self,
        entry: String,
        snapshot: agent_chat::Snapshot,
    ) -> io::Result<Receiver<Result<(), String>>> {
        self.link_agent_chat_checked(entry, snapshot, Arc::new(AtomicBool::new(true)))
    }

    /// Publishes a bounded chat copy only while its handoff permit remains true.
    pub(crate) fn link_agent_chat_checked(
        &self,
        entry: String,
        snapshot: agent_chat::Snapshot,
        permit: Arc<AtomicBool>,
    ) -> io::Result<Receiver<Result<(), String>>> {
        if !agent_chat::valid_reference(&entry) {
            return Err(super::invalid("Invalid Agent journal entry reference"));
        }
        snapshot.validate()?;
        self.enqueue_mutation(move |reply| Item::AgentChat {
            entry,
            snapshot: Box::new(snapshot),
            permit,
            reply,
        })
    }

    pub(super) fn location(
        &self,
    ) -> (
        crate::private_files::EntryIdentity,
        crate::private_files::EntryIdentity,
    ) {
        (self.root_identity, self.directory_identity)
    }

    /// Resolves an alias off the UI thread against this capture's original directory.
    pub(crate) fn matches_directory(&self, directory: &Path) -> io::Result<bool> {
        Ok(storage::directory_identity(directory)? == self.directory_identity)
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn sink(&self) -> Sink {
        self.sink.clone()
    }

    /// Replaces the profile observed by subsequent timeline appends. No disk work is performed.
    pub(crate) fn set_marker_rules(&self, rules: Arc<markers::Compiled>) {
        *self
            .sink
            .shared
            .marker_rules
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = rules;
    }

    /// Applies an explicit rule batch inside the capture FIFO, independently of live opt-in.
    pub(crate) fn apply_marker_rules(
        &self,
        source: &viewer::Source,
        rules: Arc<markers::Compiled>,
        overwrite_manual: bool,
    ) -> io::Result<Receiver<Result<MarkerOutcome, String>>> {
        self.require_entry_source(source)?;
        let source = source.clone();
        self.enqueue_mutation(move |reply| Item::ApplyMarkers {
            source,
            rules,
            overwrite_manual,
            reply,
        })
    }

    /// Queues a metadata update without interrupting capture when the queue is busy.
    /// The acknowledgement is sent after the updated document has been saved.
    pub(crate) fn edit_metadata(
        &self,
        edit: MetadataEdit,
    ) -> io::Result<Receiver<Result<(), String>>> {
        let edit = edit.normalized()?;
        self.enqueue_mutation(move |reply| Item::Metadata { edit, reply })
    }

    /// Saves a previously validated entry draft in the same FIFO as capture records.
    pub(crate) fn edit_entry(
        &self,
        edit: entry_edit::Edit,
    ) -> io::Result<Receiver<Result<(), String>>> {
        self.require_entry_source(edit.source())?;
        self.enqueue_mutation(move |reply| Item::Entry {
            edit: Box::new(edit),
            reply,
        })
    }

    /// Deletes one previously verified timeline entry in the capture FIFO.
    pub(crate) fn delete_entry(
        &self,
        deletion: Arc<Deletion>,
    ) -> io::Result<Receiver<Result<Outcome, String>>> {
        self.require_entry_source(deletion.source())?;
        self.enqueue_mutation(move |reply| Item::DeleteEntry { deletion, reply })
    }

    /// Saves a reviewed screenshot and its marks in the journal writer's FIFO.
    pub(crate) fn edit_screenshot(
        &self,
        edit: impl Into<Arc<screenshot_edit::Edit>>,
    ) -> io::Result<Receiver<Result<screenshot_edit::Outcome, String>>> {
        let edit = edit.into();
        self.require_entry_source(edit.source())?;
        self.enqueue_mutation(move |reply| Item::EditScreenshot { edit, reply })
    }

    /// Queues analysis for the reviewed image, preserving other changes in the capture FIFO.
    pub(crate) fn store_screenshot_analysis(
        &self,
        asset: Arc<viewer::Screenshot>,
        analysis: Arc<screenshot_ai::Analysis>,
    ) -> io::Result<Receiver<Result<screenshot_analysis_store::Outcome, String>>> {
        self.require_entry_source(asset.source())?;
        screenshot_analysis_store::validate(&analysis)?;
        self.enqueue_mutation(move |reply| Item::ScreenshotAnalysis {
            asset,
            analysis,
            reply,
        })
    }

    fn require_entry_source(&self, source: &viewer::Source) -> io::Result<()> {
        if !source.same_journal(&viewer::Source::capture(self)) {
            return Err(super::invalid(
                "The selected session journal identity changed",
            ));
        }
        Ok(())
    }

    fn enqueue_mutation<T>(
        &self,
        item: impl FnOnce(Sender<Result<T, String>>) -> Item,
    ) -> io::Result<Receiver<Result<T, String>>> {
        let mut progress = self.sink.shared.lock();
        progress.recording().map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "The session journal is closing")
        })?;
        let (reply, result) = async_channel::bounded(1);
        let item = item(reply);
        let entry_mutation = matches!(
            &item,
            Item::Entry { .. }
                | Item::DeleteEntry { .. }
                | Item::EditScreenshot { .. }
                | Item::ScreenshotAnalysis { .. }
                | Item::ApplyMarkers { .. }
                | Item::Summary { .. }
                | Item::AgentChat { .. }
        );
        if entry_mutation && progress.entry_mutation_pending {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "A journal entry change is still being saved. Try again shortly.",
            ));
        }
        match self.sink.shared.input.try_send(item) {
            Ok(()) => {
                progress.entry_mutation_pending |= entry_mutation;
                Ok(result)
            }
            Err(TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "The session journal queue is busy. Try saving again.",
            )),
            Err(TrySendError::Closed(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "The session journal is closing",
            )),
        }
    }

    pub(crate) fn status(&self) -> Status {
        self.sink.shared.lock().status.clone()
    }

    /// Log sequence, timeline revision, and whether the capture has closed.
    pub(crate) fn published(&self) -> (u64, usize, bool) {
        let closed = self.done.is_closed();
        let shared = &self.sink.shared;
        (
            shared.sequence.load(Ordering::Acquire),
            shared.timeline.load(Ordering::Acquire),
            closed,
        )
    }

    /// The receiver closes after the final status is set. No message is sent.
    pub(crate) fn done(&self) -> Receiver<()> {
        self.done.clone()
    }

    /// Stops accepting new records and drains accepted records in FIFO order.
    pub(crate) fn finish(&self) -> Receiver<()> {
        self.sink.shared.finish(None);
        self.done()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.sink.shared.finish(None);
    }
}

#[derive(Clone)]
pub(crate) struct Sink {
    shared: Arc<Shared>,
}

impl fmt::Debug for Sink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionJournalSink").finish_non_exhaustive()
    }
}

impl Sink {
    pub(crate) fn output_paused(&self) -> bool {
        self.shared.output_paused.load(Ordering::Acquire)
    }

    /// Offers prepared, already redacted display text. Queue overflow ends capture explicitly.
    pub(crate) fn try_write(
        &self,
        kind: RecordKind,
        text: Zeroizing<String>,
        partial: bool,
    ) -> Result<(), SendError> {
        if matches!(kind, RecordKind::Output | RecordKind::Seed) && self.output_paused() {
            return Ok(());
        }
        self.queue_text(text, |text| Item::Text {
            kind,
            text,
            partial,
            time: Timestamp::now(),
        })
    }

    /// Reserves the command's place before terminal output can overtake the write.
    /// The disk worker publishes it only after the transport confirms success.
    pub(crate) fn reserve_input(
        &self,
        text: Zeroizing<String>,
    ) -> Result<mpsc::SyncSender<()>, SendError> {
        let (confirm, receipt) = mpsc::sync_channel(1);
        self.queue_text(text, |text| Item::ConfirmedInput {
            text,
            time: Timestamp::now(),
            receipt,
        })?;
        Ok(confirm)
    }

    fn queue_text(
        &self,
        text: Zeroizing<String>,
        item: impl FnOnce(Zeroizing<String>) -> Item,
    ) -> Result<(), SendError> {
        let mut progress = self.shared.lock();
        progress.recording()?;
        if text.len() > MAX_RECORD_BYTES {
            self.shared.stop(
                &mut progress,
                Some("a captured line exceeded the size limit"),
            );
            return Err(SendError::TooLarge);
        }
        let result = self.shared.input.try_send(item(text));
        self.handle_text_send(&mut progress, result)
    }

    /// Enqueues a single bounded history batch before any live activity.
    pub(crate) fn try_seed_batch(
        &self,
        lines: Vec<Zeroizing<String>>,
        older_omitted: usize,
    ) -> Result<(), SendError> {
        let mut progress = self.shared.lock();
        progress.recording()?;
        if progress.seed_sent || progress.received_text {
            return Err(SendError::Closed);
        }
        if lines.len() > MAX_SEED_LINES
            || lines.iter().any(|line| line.len() > MAX_RECORD_BYTES)
            || lines.iter().map(|line| line.len()).sum::<usize>() > MAX_SEED_BYTES
        {
            self.shared.stop(
                &mut progress,
                Some("the scrollback seed exceeded its size limit"),
            );
            return Err(SendError::TooLarge);
        }
        let result = self.shared.input.try_send(Item::Seed {
            lines,
            older_omitted,
            time: Timestamp::now(),
        });
        self.handle_text_send(&mut progress, result)?;
        progress.seed_sent = true;
        Ok(())
    }

    /// Offers one explicit screenshot without accumulating multiple queued image buffers.
    pub(crate) fn try_screenshot(&self, png: Vec<u8>) -> Result<(), SendError> {
        self.offer_screenshot(png, None)
    }

    /// Returns the published row only after both the PNG and its journal entry are stored.
    pub(crate) fn try_screenshot_with_receipt(
        &self,
        png: Vec<u8>,
    ) -> Result<Receiver<Result<PublishedScreenshot, String>>, SendError> {
        let (send, receive) = async_channel::bounded(1);
        self.offer_screenshot(png, Some(send))?;
        Ok(receive)
    }

    fn offer_screenshot(
        &self,
        png: Vec<u8>,
        receipt: Option<Sender<Result<PublishedScreenshot, String>>>,
    ) -> Result<(), SendError> {
        let png = Zeroizing::new(png);
        let mut progress = self.shared.lock();
        progress.recording()?;
        if progress.screenshot_pending {
            return Err(SendError::Full);
        }
        if !disk::valid_png(&png)
            || png.len() > MAX_SCREENSHOT_TOTAL.saturating_sub(progress.screenshot_bytes)
        {
            return Err(SendError::TooLarge);
        }
        let size = png.len();
        match self.shared.input.try_send(Item::Screenshot {
            png,
            time: Timestamp::now(),
            receipt,
        }) {
            Ok(()) => {
                progress.screenshot_pending = true;
                progress.screenshot_bytes += size;
                progress.received_text = true;
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(SendError::Full),
            Err(TrySendError::Closed(_)) => Err(SendError::Closed),
        }
    }

    /// Stops capture after a producer could not safely prepare another record.
    pub(crate) fn mark_truncated(&self) {
        self.shared
            .finish(Some("capture could not safely prepare a complete record"));
    }

    fn handle_text_send(
        &self,
        progress: &mut Progress,
        result: Result<(), TrySendError<Item>>,
    ) -> Result<(), SendError> {
        match result {
            Ok(()) => {
                progress.received_text = true;
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.shared
                    .stop(progress, Some("the disk writer queue filled"));
                Err(SendError::Full)
            }
            Err(TrySendError::Closed(_)) => Err(SendError::Closed),
        }
    }
}

enum Item {
    // Test double for one post-publication durability error in the actual writer.
    #[cfg(test)]
    FailNextMetadataSync,
    Metadata {
        edit: MetadataEdit,
        reply: Sender<Result<(), String>>,
    },
    Summary {
        update: Box<disk::SummaryPublish>,
        reply: Sender<Result<u64, String>>,
    },
    AgentCard {
        card: Box<agent_chat::Card>,
        permit: Option<Arc<AtomicBool>>,
        reply: Sender<Result<(), String>>,
    },
    AgentChat {
        entry: String,
        snapshot: Box<agent_chat::Snapshot>,
        permit: Arc<AtomicBool>,
        reply: Sender<Result<(), String>>,
    },
    Entry {
        edit: Box<entry_edit::Edit>,
        reply: Sender<Result<(), String>>,
    },
    DeleteEntry {
        deletion: Arc<Deletion>,
        reply: Sender<Result<Outcome, String>>,
    },
    EditScreenshot {
        edit: Arc<screenshot_edit::Edit>,
        reply: Sender<Result<screenshot_edit::Outcome, String>>,
    },
    ScreenshotAnalysis {
        asset: Arc<viewer::Screenshot>,
        analysis: Arc<screenshot_ai::Analysis>,
        reply: Sender<Result<screenshot_analysis_store::Outcome, String>>,
    },
    ApplyMarkers {
        source: viewer::Source,
        rules: Arc<markers::Compiled>,
        overwrite_manual: bool,
        reply: Sender<Result<MarkerOutcome, String>>,
    },
    Text {
        kind: RecordKind,
        text: Zeroizing<String>,
        partial: bool,
        time: Timestamp,
    },
    ConfirmedInput {
        text: Zeroizing<String>,
        time: Timestamp,
        receipt: mpsc::Receiver<()>,
    },
    Seed {
        lines: Vec<Zeroizing<String>>,
        older_omitted: usize,
        time: Timestamp,
    },
    Screenshot {
        png: Zeroizing<Vec<u8>>,
        time: Timestamp,
        receipt: Option<Sender<Result<PublishedScreenshot, String>>>,
    },
}

struct Shared {
    output_paused: AtomicBool,
    input: Sender<Item>,
    marker_rules: Mutex<Arc<markers::Compiled>>,
    progress: Mutex<Progress>,
    sequence: AtomicU64,
    timeline: AtomicUsize,
}

impl Shared {
    fn marker_rules(&self) -> Arc<markers::Compiled> {
        Arc::clone(
            &self
                .marker_rules
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    fn publish(&self, sequence: u64, timeline: usize) {
        self.sequence.store(sequence, Ordering::Release);
        self.timeline.store(timeline, Ordering::Release);
    }

    fn lock(&self) -> MutexGuard<'_, Progress> {
        self.progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn finish(&self, reason: Option<&'static str>) {
        let mut progress = self.lock();
        self.stop(&mut progress, reason);
    }

    fn stop(&self, progress: &mut Progress, reason: Option<&'static str>) {
        if matches!(progress.status, Status::Recording) {
            progress.status = Status::Finishing;
            progress.truncation = reason;
            self.input.close();
        }
    }
}

struct Progress {
    status: Status,
    truncation: Option<&'static str>,
    received_text: bool,
    seed_sent: bool,
    screenshot_pending: bool,
    entry_mutation_pending: bool,
    screenshot_bytes: usize,
}

impl Progress {
    fn recording(&self) -> Result<(), SendError> {
        if matches!(self.status, Status::Recording) {
            Ok(())
        } else {
            Err(SendError::Closed)
        }
    }
}

struct Completion {
    shared: Arc<Shared>,
    notification: Sender<()>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        self.shared.input.close();
        let mut progress = self.shared.lock();
        if matches!(progress.status, Status::Recording | Status::Finishing) {
            progress.status =
                Status::Failed("The session journal writer stopped unexpectedly.".into());
        }
        drop(progress);
        self.notification.close();
    }
}

pub(super) fn uuid() -> io::Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
