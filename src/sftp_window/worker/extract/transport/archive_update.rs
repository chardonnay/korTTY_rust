use std::fs::File;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kortty_server_archive::protocol::{
    self, CreateSpec, CreatedPayload, UpdateDecision, UpdateReply, UpdateRequest,
};
use kortty_server_archive::rsync_tree::valid_stage;
use kortty_ssh::{ExecLease, ExecOpener, SensitiveLine, SshReader, SshWriter};

use super::{
    DECISION_TIMEOUT, PrivilegedReader, invalid_output, open, open_privileged_helper,
    receive_archive_copy, require_privileged_eof, send, submit, take_io,
};

const PREPARE_TIMEOUT: Duration = Duration::from_secs(1800);

pub(crate) struct ArchiveUpdatePublication {
    pub(crate) payload: CreatedPayload,
    pub(crate) recovery: Option<String>,
    pub(crate) copy_verified: bool,
}

#[derive(Debug)]
pub(crate) enum ArchiveUpdateFailure {
    BeforePublication,
    Refused,
    Uncertain { recovery: Option<String> },
}

pub(crate) fn exchange_archive_update(
    opener: &ExecOpener,
    spec: &CreateSpec,
    password: Option<&str>,
    stopped: impl Fn() -> bool + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Send + Sync + 'static,
) -> Result<ArchiveUpdatePublication, ArchiveUpdateFailure> {
    exchange(opener, spec, password, None, stopped, authorize)
}

pub(crate) fn exchange_privileged_archive_update(
    opener: &ExecOpener,
    spec: &CreateSpec,
    sudo_password: Option<SensitiveLine>,
    password: Option<&str>,
    copy: Option<&File>,
    stopped: impl Fn() -> bool + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Send + Sync + 'static,
) -> Result<ArchiveUpdatePublication, ArchiveUpdateFailure> {
    if spec.report_payload != copy.is_some() {
        return Err(ArchiveUpdateFailure::BeforePublication);
    }
    exchange(
        opener,
        spec,
        password,
        Some(PrivilegedUpdate {
            sudo_password,
            copy,
        }),
        stopped,
        authorize,
    )
}

struct PrivilegedUpdate<'a> {
    sudo_password: Option<SensitiveLine>,
    copy: Option<&'a File>,
}

fn exchange(
    opener: &ExecOpener,
    spec: &CreateSpec,
    password: Option<&str>,
    privileged: Option<PrivilegedUpdate<'_>>,
    stopped: impl Fn() -> bool + Send + Sync + 'static,
    authorize: impl Fn() -> io::Result<()> + Send + Sync + 'static,
) -> Result<ArchiveUpdatePublication, ArchiveUpdateFailure> {
    use ArchiveUpdateFailure::{BeforePublication, Refused, Uncertain};

    if (spec.format == protocol::CreateFormat::ZipPassword) != password.is_some() {
        return Err(BeforePublication);
    }
    let password = password
        .map(protocol::creation_password_body)
        .transpose()
        .map_err(|_| BeforePublication)?;
    let deadline = Instant::now() + PREPARE_TIMEOUT;
    let permitted =
        Arc::new(move || !stopped() && Instant::now() < deadline && authorize().is_ok());
    let in_one_channel = privileged.is_some();
    let (mut channel, copy) = match privileged {
        Some(PrivilegedUpdate {
            sudo_password,
            copy,
        }) => {
            let channel = open_privileged_helper(
                opener,
                (
                    protocol::PRIVILEGED_UPDATE_ARGUMENT,
                    protocol::PRIVILEGED_UPDATE_PASSWORD_ARGUMENT,
                ),
                protocol::PRIVILEGED_UPDATE_PASSWORD_READY,
                password.is_some(),
                sudo_password,
                permitted.clone(),
            )
            .map_err(|_| BeforePublication)?;
            (channel, copy)
        }
        None => (
            open(opener, protocol::UPDATE_COMMAND, false, permitted.clone())
                .and_then(take_io)
                .map_err(|_| BeforePublication)?,
            None,
        ),
    };
    let review = {
        let (lease, stdout, input) = &mut channel;
        let mut frame = Vec::new();
        protocol::write_update_request(&mut frame, &UpdateRequest::Review { spec: spec.clone() })
            .map_err(|_| BeforePublication)?;
        submit(input, &frame, permitted.clone()).map_err(|_| BeforePublication)?;
        let stopped = || !permitted();
        let mut reader = PrivilegedReader {
            stdout,
            stopped: &stopped,
        };
        let reply = protocol::read_update_reply(&mut reader).map_err(|_| BeforePublication)?;
        if !in_one_channel {
            require_privileged_eof(&mut reader, || lease.exit_code(), &stopped)
                .map_err(|_| BeforePublication)?;
        }
        reply
    };
    let UpdateReply::Reviewed {
        review,
        existing,
        receiver,
    } = review
    else {
        return Err(Refused);
    };
    review.validate_for(spec).map_err(|_| BeforePublication)?;
    if receiver == [0; 32] || existing.is_some_and(|payload| payload.validate().is_err()) {
        return Err(BeforePublication);
    }

    if !in_one_channel {
        drop(channel);
        channel = open(
            opener,
            protocol::UPDATE_COMMAND,
            password.is_some(),
            permitted.clone(),
        )
        .and_then(take_io)
        .map_err(|_| BeforePublication)?;
    }
    let (lease, mut stdout, mut input) = channel;
    let mut frame = Vec::new();
    protocol::write_update_request(
        &mut frame,
        &UpdateRequest::Prepare {
            spec: spec.clone(),
            token: review.token,
        },
    )
    .map_err(|_| BeforePublication)?;
    submit(&mut input, &frame, permitted.clone()).map_err(|_| BeforePublication)?;
    let preparing = || !permitted();
    let mut reply = read(&mut stdout, &preparing).map_err(|_| BeforePublication)?;
    if let Some(password) = password {
        match reply {
            UpdateReply::PasswordRequired => {}
            UpdateReply::Failed { .. } => return Err(Refused),
            _ => return Err(BeforePublication),
        }
        let abort = lease.abort_handle();
        let secret_permitted = permitted.clone();
        send(
            &preparing,
            move || abort.abort(),
            || input.send_sensitive_frame(password, move || secret_permitted()),
        )
        .map_err(|_| BeforePublication)?;
        reply = read(&mut stdout, &preparing).map_err(|_| BeforePublication)?;
    }
    let UpdateReply::Prepared { payload } = reply else {
        return Err(Refused);
    };
    payload.validate().map_err(|_| BeforePublication)?;
    if !permitted() {
        return Err(BeforePublication);
    }

    let recovery = if existing.is_some() {
        decide(&mut input, UpdateDecision::Quarantine, permitted.clone())
            .map_err(|_| Uncertain { recovery: None })?;
        let deadline = Instant::now() + DECISION_TIMEOUT;
        let waiting = || Instant::now() >= deadline;
        let mut reader = PrivilegedReader {
            stdout: &mut stdout,
            stopped: &waiting,
        };
        match protocol::read_update_reply(&mut reader) {
            Ok(UpdateReply::Quarantined { recovery }) if valid_recovery(spec, &recovery) => {
                Some(recovery)
            }
            Ok(reply) => return Err(terminal_failure(spec, reply, None, &mut reader, &lease)),
            Err(_) => return Err(Uncertain { recovery: None }),
        }
    } else {
        None
    };

    // Keep reading the final result after a submitted mutation, even if the job was cancelled.
    let deadline = Instant::now() + DECISION_TIMEOUT;
    let ending = move || Instant::now() >= deadline;
    let authorized = permitted();
    let (decision, decision_permitted): (_, Arc<dyn Fn() -> bool + Send + Sync>) = if authorized {
        (UpdateDecision::Publish, permitted.clone())
    } else if recovery.is_some() {
        (UpdateDecision::Abort, Arc::new(move || !ending()))
    } else {
        return Err(BeforePublication);
    };
    decide(&mut input, decision, decision_permitted).map_err(|_| Uncertain {
        recovery: recovery.clone(),
    })?;
    let mut reader = PrivilegedReader {
        stdout: &mut stdout,
        stopped: &ending,
    };
    let reply = protocol::read_update_reply(&mut reader).map_err(|_| Uncertain {
        recovery: recovery.clone(),
    })?;
    match reply {
        UpdateReply::Published {
            destination,
            payload: published,
            recovery: retained,
            receiver: successor,
        } if authorized
            && destination == spec.destination
            && published == payload
            && retained == recovery
            && successor != [0; 32]
            && successor != receiver =>
        {
            let copy_verified = match copy {
                Some(file) => {
                    receive_copy(&lease, &mut stdout, &mut input, file, payload, permitted).is_ok()
                }
                None => {
                    require_privileged_eof(&mut reader, || lease.exit_code(), &ending).map_err(
                        |_| Uncertain {
                            recovery: recovery.clone(),
                        },
                    )?;
                    false
                }
            };
            Ok(ArchiveUpdatePublication {
                payload,
                recovery,
                copy_verified,
            })
        }
        UpdateReply::Aborted => {
            require_privileged_eof(&mut reader, || lease.exit_code(), &ending).map_err(|_| {
                Uncertain {
                    recovery: recovery.clone(),
                }
            })?;
            Err(BeforePublication)
        }
        reply => Err(terminal_failure(
            spec,
            reply,
            recovery.as_deref(),
            &mut reader,
            &lease,
        )),
    }
}

fn receive_copy(
    lease: &ExecLease,
    stdout: &mut SshReader,
    input: &mut SshWriter,
    mut file: &File,
    payload: CreatedPayload,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> io::Result<()> {
    decide(input, UpdateDecision::FetchCopy, permitted.clone())?;
    let stopped = || !permitted();
    let mut reader = PrivilegedReader {
        stdout,
        stopped: &stopped,
    };
    match protocol::read_update_reply(&mut reader)? {
        UpdateReply::CopyReady { payload: copy } if copy == payload => {
            receive_archive_copy(&mut reader, &mut file, payload)?;
            require_privileged_eof(&mut reader, || lease.exit_code(), &stopped)
        }
        _ => Err(invalid_output()),
    }
}

fn terminal_failure(
    spec: &CreateSpec,
    reply: UpdateReply,
    known: Option<&str>,
    reader: &mut PrivilegedReader<'_, impl Fn() -> bool>,
    lease: &ExecLease,
) -> ArchiveUpdateFailure {
    if let UpdateReply::Failed {
        outcome_unknown,
        recovery,
        ..
    } = reply
    {
        let stopped = reader.stopped;
        if !outcome_unknown
            && recovery.is_none()
            && require_privileged_eof(reader, || lease.exit_code(), stopped).is_ok()
        {
            return ArchiveUpdateFailure::Refused;
        }
        return ArchiveUpdateFailure::Uncertain {
            recovery: recovery.filter(|path| {
                valid_recovery(spec, path) && known.is_none_or(|known| known == path)
            }),
        };
    }
    ArchiveUpdateFailure::Uncertain { recovery: None }
}

fn valid_recovery(spec: &CreateSpec, path: &str) -> bool {
    let Some((parent, _)) = spec.destination.rsplit_once('/') else {
        return false;
    };
    let Some((directory, entry)) = path.rsplit_once('/') else {
        return false;
    };
    entry == "original"
        && directory
            .rsplit_once('/')
            .is_some_and(|(actual, stage)| actual == parent && valid_stage(stage))
}

fn read(stdout: &mut SshReader, stopped: &impl Fn() -> bool) -> io::Result<UpdateReply> {
    protocol::read_update_reply(PrivilegedReader { stdout, stopped })
}

fn decide(
    input: &mut SshWriter,
    decision: UpdateDecision,
    permitted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> io::Result<()> {
    let mut frame = Vec::new();
    protocol::write_update_decision(&mut frame, &decision)?;
    submit(input, &frame, permitted)
}
