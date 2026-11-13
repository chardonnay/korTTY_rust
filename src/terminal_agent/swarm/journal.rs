//! Publishes explicitly selected, already redacted per-target activity.

use std::io;
use std::sync::Arc;

use crate::session_journal::automation::{
    Artifact, CaptureSettings, Destination, Recorder, Run, identity_digest,
};
use crate::session_journal::tree::RunStatus;
use crate::terminal_logging::{Policy, Redactor, Secrets};

use super::{Row, Status};

pub(crate) fn publish(
    settings: &CaptureSettings,
    run: Run,
    row: &Row,
    policy: Policy,
) -> io::Result<Artifact> {
    let status = match row.status {
        Status::Succeeded => RunStatus::Success,
        Status::Failed => RunStatus::Failed,
        Status::Blocked => RunStatus::Blocked,
        Status::Cancelled => RunStatus::Cancelled,
        _ => return Err(unavailable()),
    };
    if row.target.private {
        return Err(unavailable());
    }
    let identity = if row.attempt() == 1 {
        format!("{:?}", row.target.key)
    } else {
        format!("{:?} / attempt {}", row.target.key, row.attempt())
    };
    let recorder = Recorder::start(
        settings,
        run,
        Destination {
            key: identity_digest(identity.as_bytes())?,
            label: row.target.label.clone(),
            host: String::new(),
            port: None,
            username: String::new(),
        },
        Arc::new(Redactor::for_journal(Secrets::default().seal()?, policy)),
    )?;
    recorder.output(&row.transcript, false);
    recorder.output(&row.detail, false);
    Ok(recorder.finish(status, (status == RunStatus::Success).then_some(0)))
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Only completed non-private swarm activity can be journaled",
    )
}
