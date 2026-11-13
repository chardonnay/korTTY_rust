//! Replay the physical records of one command without buffering its complete output.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pgp::crypto::hash::HashAlgorithm;
use rsa::signature::digest::DynDigest;

use super::super::CapturePage;
use super::super::summary_job::{MAX_PAGES, capture_has_gap, check_active};
use super::super::viewer::Source;
use super::{Cursor, Line, Observation, ObservationKind, SensitiveBlocks, Window};

pub(super) struct Start {
    pub(super) cursor: Option<Cursor>,
    pub(super) sensitive: SensitiveBlocks,
}

const PROOF_BYTES: usize = 64 * 1024;
const PROOF_RECORDS: usize = 128;

pub(super) struct Builder {
    start: Arc<Start>,
    proofs: Vec<Proof>,
    hash: Box<dyn DynDigest + Send>,
    bytes: usize,
    records: usize,
}

impl Builder {
    pub(super) fn new(start: Arc<Start>) -> Self {
        Self {
            start,
            proofs: Vec::new(),
            hash: hasher(),
            bytes: 0,
            records: 0,
        }
    }

    pub(super) fn record(&mut self, observation: &Observation) {
        if self.records == PROOF_RECORDS
            || self.records > 0 && self.bytes + observation.text.len() > PROOF_BYTES
        {
            self.proofs.push(Proof {
                records: self.records,
                fingerprint: digest(&self.hash.finalize_reset()),
            });
            self.bytes = 0;
            self.records = 0;
        }
        observation.fingerprint(self.hash.as_mut());
        self.bytes += observation.text.len();
        self.records += 1;
    }

    pub(super) fn seal(&self) -> LongOutput {
        let mut proofs = self.proofs.clone();
        if self.records != 0 {
            proofs.push(Proof {
                records: self.records,
                fingerprint: digest(&self.hash.box_clone().finalize()),
            });
        }
        LongOutput {
            start: Arc::clone(&self.start),
            proofs: proofs.into(),
        }
    }
}

#[derive(Clone, Copy)]
struct Proof {
    records: usize,
    fingerprint: [u8; 32],
}

#[derive(Clone)]
pub(in crate::session_journal) struct LongOutput {
    start: Arc<Start>,
    proofs: Arc<[Proof]>,
}

impl LongOutput {
    pub(in crate::session_journal) fn visit(
        &self,
        source: &Source,
        window: &Window,
        cancelled: &AtomicBool,
        visit: impl FnMut(Line) -> io::Result<()>,
    ) -> io::Result<()> {
        self.visit_pages(
            window,
            cancelled,
            |cursor| source.capture_page(cursor, None, None),
            visit,
        )
    }

    fn visit_pages(
        &self,
        window: &Window,
        cancelled: &AtomicBool,
        mut read_page: impl FnMut(Option<Cursor>) -> io::Result<CapturePage>,
        mut visit: impl FnMut(Line) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut cursor = self.start.cursor;
        let mut sensitive = self.start.sensitive.clone();
        let mut newest = window.first.saturating_sub(1);
        let mut proofs = self.proofs.iter().peekable();
        let mut fingerprint = hasher();
        let mut pending = Vec::with_capacity(PROOF_RECORDS);
        let mut bytes = 0;
        for _ in 0..MAX_PAGES {
            check_active(cancelled)?;
            let page = read_page(cursor).map_err(|_| changed())?;
            sensitive.uncertain |= capture_has_gap(&page.status);
            for record in page.records {
                check_active(cancelled)?;
                let record = sensitive.mask(record);
                let Some(observation) = Observation::from_record(record, true) else {
                    continue;
                };
                if observation.sequence <= newest {
                    continue;
                }
                if observation.sequence > window.last {
                    return Err(changed());
                }
                newest = observation.sequence;
                observation.fingerprint(fingerprint.as_mut());
                bytes += observation.text.len();
                if !pending.is_empty() && bytes > PROOF_BYTES {
                    return Err(changed());
                }
                pending.push(observation);
                let proof = proofs.peek().ok_or_else(changed)?;
                if pending.len() == proof.records {
                    if digest(&fingerprint.finalize_reset()) != proof.fingerprint {
                        return Err(changed());
                    }
                    proofs.next();
                    bytes = 0;
                    for observation in pending.drain(..) {
                        check_active(cancelled)?;
                        if let ObservationKind::Line(kind) = observation.kind {
                            visit(observation.line(kind))?;
                        }
                    }
                }
                if newest == window.last {
                    return if pending.is_empty() && proofs.peek().is_none() {
                        Ok(())
                    } else {
                        Err(changed())
                    };
                }
            }
            let Some(next) = page.next else { break };
            if cursor == Some(next) {
                break;
            }
            cursor = Some(next);
        }
        Err(changed())
    }
}

fn hasher() -> Box<dyn DynDigest + Send> {
    HashAlgorithm::Sha256
        .new_hasher()
        .expect("built-in SHA-256")
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("SHA-256 digest length")
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "The journal capture changed before the complete command was read",
    )
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::session_journal::summary_plan::Planner;
    use crate::session_journal::{CaptureKind, CaptureRecord};

    #[test]
    fn journal_summary_replay_checks_each_group_before_releasing_evidence() {
        let at = Timestamp::from_second(1).unwrap();
        let records: Vec<_> = (1..=151)
            .map(|sequence| CaptureRecord {
                sequence,
                timestamp: at,
                kind: if sequence == 1 {
                    CaptureKind::Input
                } else {
                    CaptureKind::Output
                },
                text: if sequence == 1 {
                    "inspect output".into()
                } else {
                    format!("trusted line {sequence}")
                },
                redacted: false,
                partial: false,
                repeat: 1,
            })
            .collect();
        let mut planner = Planner::resume(0);
        planner.begin_page(None, true);
        for record in &records {
            assert!(planner.push(record.clone()).is_none());
        }
        let window = planner.finish_pass(true, true, at).pop().unwrap();
        let replay = window.long_output.as_ref().unwrap();
        let stopped = AtomicBool::new(false);
        for (change, expected) in [(None, 151), (Some(2), 0), (Some(130), PROOF_RECORDS)] {
            let mut edited = records.clone();
            if let Some(index) = change {
                edited[index].text = "replacement never approved".into();
            }
            let mut released = Vec::new();
            let result = replay.visit_pages(
                &window,
                &stopped,
                |_| {
                    Ok(CapturePage {
                        records: std::mem::take(&mut edited),
                        ..CapturePage::default()
                    })
                },
                |line| {
                    released.push(line.text);
                    Ok(())
                },
            );
            if change.is_some() {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            } else {
                result.unwrap();
            }
            assert_eq!(released.len(), expected);
            assert!(released.iter().all(|line| !line.contains("replacement")));
        }
    }
}
