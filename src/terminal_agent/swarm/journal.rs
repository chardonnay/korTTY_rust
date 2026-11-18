//! Publishes explicitly selected, already redacted per-target activity.

use std::io;
use std::sync::Arc;

use crate::session_journal::automation::{
    Artifact, CaptureSettings, Destination, Recorder, Run, identity_digest,
};
use crate::session_journal::tree::RunStatus;
use crate::terminal_logging::Redactor;

use super::{Row, Status};

pub(crate) enum Activity {
    Command(String),
    Output(String),
}

pub(crate) struct Evidence {
    pub(crate) activity: Vec<Activity>,
    pub(crate) truncated: bool,
}

pub(crate) fn publish(
    settings: &CaptureSettings,
    run: Run,
    row: &Row,
    evidence: Evidence,
    redactor: Arc<Redactor>,
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
        redactor,
    )?
    .ordered_commands();
    for entry in evidence.activity {
        match entry {
            Activity::Command(command) => recorder.command(&command),
            Activity::Output(output) => recorder.output(&output, false),
        }
    }
    recorder.output(&row.detail, false);
    if evidence.truncated {
        Ok(recorder.finish_status(status, true))
    } else {
        Ok(recorder.finish(status, (status == RunStatus::Success).then_some(0)))
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Only completed non-private swarm activity can be journaled",
    )
}
