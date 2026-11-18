//! Review and apply a protected merge through the pinned Linux helper.

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Instant;

use kortty_server_archive::privileged_file_replace::valid_recovery;
use kortty_server_archive::privileged_tree_merge::{
    self as wire, Change, Decision, Reply, Request, Review,
};

use crate::sftp_window::scheduled::{Preview, ReplacementPreview};
use crate::sftp_window::worker::extract::transport::{DECISION_TIMEOUT, FixedHelper, Permit};

use super::super::MergeReport;
use super::{Auth, ExecOpener, Failure, TreeUpload, copy_digest, digest_hex, open_file};

pub(crate) fn preview_tree_upload_merge(
    opener: ExecOpener,
    root: &str,
    plan: &TreeUpload,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<(Preview, Option<[u8; 32]>), Failure> {
    let allowed: Permit = Arc::new(allowed);
    let mut exchange = Exchange::begin(
        opener,
        root,
        plan,
        auth,
        &allowed,
        true,
        plan.receiver_receipt,
    )?;
    exchange
        .helper
        .finish(&|| !allowed())
        .map_err(|_| Failure::Failed)?;
    plan.verify(&|| allowed())?;
    Ok((exchange.preview(plan)?, exchange.review.receiver))
}

pub(crate) fn upload_tree_merge(
    opener: ExecOpener,
    root: &str,
    plan: &TreeUpload,
    auth: Auth,
    allowed: impl Fn() -> bool + Send + Sync + 'static,
    claim: impl FnOnce(super::super::MergeClaim) -> Result<(), Failure>,
    publication_allowed: impl Fn() -> bool,
) -> Result<MergeReport, Failure> {
    let allowed: Permit = Arc::new(allowed);
    let mut exchange = Exchange::begin(
        opener,
        root,
        plan,
        auth,
        &allowed,
        false,
        plan.receiver_receipt,
    )?;
    plan.verify(&|| allowed())?;
    let preview = exchange.preview(plan)?;
    claim(super::super::MergeClaim::from_preview(
        &preview,
        exchange.review.receiver,
    ))?;
    let prefix = exchange.transfer(plan, &allowed)?;
    plan.verify(&|| allowed())?;

    let mut report = MergeReport::default();
    for group_index in 0..exchange.groups.len() {
        let index = if exchange.request.create_root {
            0
        } else {
            exchange.groups[group_index][0]
        };
        let replace = !exchange.request.create_root
            && matches!(exchange.review.changes[index], Change::Replace { .. });
        let result = exchange.publish(index, &prefix, &allowed, &publication_allowed);
        if replace && matches!(result, Ok(()) | Err(Failure::Uncertain)) {
            report.recovery_prefix.get_or_insert_with(|| prefix.clone());
        }
        if let Err(error) = result {
            report.error = Some(error);
            return Ok(report);
        }
        let group = &exchange.groups[group_index];
        if replace {
            report.replaced += 1;
        } else {
            report.created += group.len() + usize::from(exchange.request.create_root);
        }
        report.bytes += group
            .iter()
            .filter_map(|index| exchange.request.entries[*index].file)
            .map(|file| file.bytes)
            .sum::<u64>();
    }

    let deadline = Instant::now() + DECISION_TIMEOUT;
    let stopped = || Instant::now() >= deadline;
    let confirmed = match wire::read(exchange.helper.reader(&stopped)) {
        Ok(Reply::Complete { receiver })
            if if exchange.request.create_root {
                receiver.is_some_and(|value| value != [0; 32])
                    && receiver != exchange.review.receiver
            } else {
                receiver == exchange.review.receiver
            } =>
        {
            Some(receiver)
        }
        _ => None,
    };
    if confirmed.is_none() || exchange.helper.finish(&stopped).is_err() {
        report.error = Some(Failure::Uncertain);
    } else {
        report.receiver = confirmed.flatten();
    }
    Ok(report)
}

struct Exchange {
    helper: FixedHelper,
    request: Request,
    review: Review,
    groups: Vec<Vec<usize>>,
}

impl Exchange {
    fn begin(
        opener: ExecOpener,
        root: &str,
        plan: &TreeUpload,
        auth: Auth,
        allowed: &Permit,
        review_only: bool,
        receiver_receipt: bool,
    ) -> Result<Self, Failure> {
        let request = Request {
            root: root.to_owned(),
            entries: plan
                .entries
                .iter()
                .map(|entry| wire::Entry {
                    path: entry.path.clone(),
                    file: entry.file,
                })
                .collect(),
            review_only,
            receiver_receipt,
            create_root: plan.create_root,
        };
        let ExecOpener::Direct(opener) = opener else {
            return Err(Failure::Blocked);
        };
        if !request.valid() {
            return Err(Failure::Blocked);
        }
        let password = match auth {
            Auth::Passwordless => None,
            Auth::Password(line) => Some(line),
        };
        let mut helper = FixedHelper::open(
            &opener,
            wire::ARGUMENT,
            wire::READY,
            password,
            allowed.clone(),
        )
        .map_err(|_| Failure::Blocked)?;
        send(&mut helper, &request, allowed).map_err(|_| Failure::Failed)?;
        let Ok(Reply::Reviewed { review }) = wire::read(helper.reader(&|| !allowed())) else {
            return Err(Failure::Blocked);
        };
        let groups = review.publications(&request).ok_or(Failure::Failed)?;
        Ok(Self {
            helper,
            request,
            review,
            groups,
        })
    }

    fn preview(&self, plan: &TreeUpload) -> Result<Preview, Failure> {
        let mut preview = Preview {
            proof: plan.proof(&digest_hex(&self.review.fingerprint)?)?,
            roots: plan.roots.clone(),
            paths: Vec::new(),
            replacements: Vec::new(),
            bytes: 0,
        };
        if self.request.create_root {
            preview.paths.push((true, "Receiver root".into()));
        }
        for (entry, change) in plan.entries.iter().zip(&self.review.changes) {
            let path = entry.relative.review_label();
            match change {
                Change::Keep => continue,
                Change::Create => preview.paths.push((entry.file.is_none(), path)),
                Change::Replace { original } => {
                    let source = entry.file.ok_or(Failure::Failed)?;
                    preview.replacements.push(ReplacementPreview {
                        source: path.clone(),
                        target: path,
                        source_sha256: digest_hex(&source.sha256)?,
                        target_sha256: digest_hex(&original.sha256)?,
                    });
                }
            }
            preview.bytes += entry.file.map_or(0, |file| file.bytes);
        }
        Ok(preview)
    }

    fn transfer(&mut self, plan: &TreeUpload, allowed: &Permit) -> Result<String, Failure> {
        send(&mut self.helper, &Decision::Receive, allowed).map_err(|_| Failure::Failed)?;
        let stopped = || !allowed();
        if !matches!(wire::read(self.helper.reader(&stopped)), Ok(Reply::Receive)) {
            return Err(Failure::Conflict);
        }
        let mut output = PayloadWriter {
            helper: &mut self.helper,
            allowed,
        };
        for group in &self.groups {
            for index in group {
                let entry = &plan.entries[*index];
                if let Some(file) = entry.file {
                    let source = open_file(&plan.manifest, &entry.relative)?;
                    let digest =
                        copy_digest(&mut source.file(), &mut output, file.bytes, || allowed())?;
                    if digest != file.sha256 || source.verify().is_err() {
                        return Err(Failure::Conflict);
                    }
                }
            }
        }
        let Ok(Reply::Verified { recovery_prefix }) = wire::read(self.helper.reader(&stopped))
        else {
            return Err(Failure::Conflict);
        };
        digest_hex(&recovery_prefix)
    }

    fn publish(
        &mut self,
        index: usize,
        prefix: &str,
        allowed: &Permit,
        publication_allowed: &impl Fn() -> bool,
    ) -> Result<(), Failure> {
        if !allowed() || !publication_allowed() {
            return Err(Failure::Blocked);
        }
        send(&mut self.helper, &Decision::Publish, allowed).map_err(|_| Failure::Uncertain)?;
        // Read the current outcome and allow restoration even after authorization is revoked.
        let deadline = Instant::now() + DECISION_TIMEOUT;
        let waiting: Permit = Arc::new(move || Instant::now() < deadline);
        let stopped = || !waiting();
        let mut reply = wire::read(self.helper.reader(&stopped)).map_err(|_| Failure::Uncertain)?;
        let mut authorized = true;
        if !self.request.create_root && matches!(self.review.changes[index], Change::Replace { .. })
        {
            let destination = self.request.destination(&self.request.entries[index]);
            match reply {
                Reply::Quarantined {
                    index: current,
                    recovery,
                } if current == index
                    && valid_recovery(&destination, &recovery)
                    && recovery
                        .rsplit('/')
                        .nth(1)
                        .and_then(|stage| stage.strip_prefix(".kortty-"))
                        .is_some_and(|nonce| nonce.starts_with(prefix)) =>
                {
                    authorized = allowed() && publication_allowed();
                    if authorized {
                        send(&mut self.helper, &Decision::Publish, allowed)
                            .map_err(|_| Failure::Uncertain)?;
                    } else {
                        send(&mut self.helper, &Decision::Abort, &waiting)
                            .map_err(|_| Failure::Uncertain)?;
                    }
                    reply =
                        wire::read(self.helper.reader(&stopped)).map_err(|_| Failure::Uncertain)?;
                }
                _ => return Err(self.refusal(reply, &stopped, authorized)),
            }
        }

        if authorized && matches!(reply, Reply::Applied { index: current } if index == current) {
            Ok(())
        } else {
            Err(self.refusal(reply, &stopped, authorized))
        }
    }

    fn refusal(&mut self, reply: Reply, stopped: &impl Fn() -> bool, authorized: bool) -> Failure {
        if matches!(
            reply,
            Reply::Aborted
                | Reply::Failed {
                    outcome_unknown: false,
                    recovery: None
                }
        ) && self.helper.finish(stopped).is_ok()
        {
            if authorized {
                Failure::Conflict
            } else {
                Failure::Blocked
            }
        } else {
            Failure::Uncertain
        }
    }
}

fn send(
    helper: &mut FixedHelper,
    message: &impl serde::Serialize,
    allowed: &Permit,
) -> io::Result<()> {
    let mut frame = Vec::new();
    wire::write(&mut frame, message)?;
    helper.write(&frame, allowed.clone())
}

struct PayloadWriter<'a> {
    helper: &'a mut FixedHelper,
    allowed: &'a Permit,
}

impl Write for PayloadWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.helper.write(bytes, self.allowed.clone())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
