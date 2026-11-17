//! Inline file upload through the same protected receiver as elevated rsync.

use std::io::{self, Read, Write};
use std::time::Duration;

use kortty_server_archive::privileged_file_replace::{Decision, Payload, READY, Reply, Request};
use kortty_server_archive::rsync_tree::{Manifest, read_frame, write_frame};
use rustix::fs::{Mode, OFlags, fchmod, openat};
use rustix::process::umask;
use sha2::{Digest, Sha256};

use super::Rules;
use crate::server::rsync_tree::{ExistingFile, ExistingRecovery, RECEIVED_FILE, UploadStage};
use crate::server::{File, PRIVATE_FILE, absolute_file, invalid};

pub(crate) fn serve() -> io::Result<()> {
    let mut output = io::stdout().lock();
    let mut recovery = ExistingRecovery::default();
    let result = (|| {
        umask(Mode::from_bits_truncate(0o077));
        let rules = Rules::load_inline_upload()?;
        std::thread::Builder::new()
            .name("kortty-protected-file-deadline".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(3600));
                std::process::exit(124);
            })?;
        output.write_all(READY)?;
        output.flush()?;
        exchange(&rules, &mut io::stdin().lock(), &mut output, &mut recovery)
    })();
    match result {
        Ok(Some(final_reply)) => write_frame(output, &final_reply),
        Ok(None) => Ok(()),
        Err(_) => write_frame(
            output,
            &Reply::Failed {
                outcome_unknown: recovery.uncertain,
                recovery: recovery.path,
            },
        ),
    }
}

fn exchange(
    rules: &Rules,
    input: &mut impl Read,
    output: &mut impl Write,
    recovery: &mut ExistingRecovery,
) -> io::Result<Option<Reply>> {
    let Request {
        path,
        review_only,
        recurring,
    } = read_frame(&mut *input)?;
    if path.len() > 2048 {
        return Err(invalid("The protected file path exceeds its size limit"));
    }
    let (parent, _) = absolute_file(&path)?;
    let target = ExistingFile::open_transfer_at(&path, rules.destination_parent(parent, true)?)?;
    let receiver = target.review();
    rules.require_current()?;
    write_frame(
        &mut *output,
        &Reply::Reviewed {
            fingerprint: scoped_fingerprint(rules, receiver.fingerprint),
            bytes: receiver.inventory.bytes,
        },
    )?;
    if review_only {
        return Ok(None);
    }

    let payload: Payload = match read_frame(&mut *input) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    };
    if !payload.valid() {
        return Err(invalid("The checked file exceeds the transfer size limit"));
    }
    rules.require_current()?;
    target.require_current()?;
    let mut stage = UploadStage::create_checked(target.parent())?;
    let mut file = File::from(openat(
        &stage.payload.fd,
        RECEIVED_FILE,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        PRIVATE_FILE,
    )?);
    fchmod(&file, PRIVATE_FILE)?;
    write_frame(&mut *output, &Reply::Receive)?;
    if io::copy(&mut (&mut *input).take(payload.bytes), &mut file)? != payload.bytes {
        return Err(invalid("The upload ended before its declared size"));
    }
    drop(file);
    let expected = Manifest::regular_file(payload.bytes, payload.sha256);
    let successor = target.prepare_successor(&stage, expected)?;
    rules.require_current()?;
    write_frame(&mut *output, &Reply::Verified { payload })?;
    match read_frame(&mut *input)? {
        Decision::Abort => return Ok(Some(Reply::Aborted)),
        Decision::Quarantine => {}
        _ => {
            return Err(invalid(
                "The existing file must be quarantined before publication",
            ));
        }
    }
    let published = target.commit_checked(
        &mut stage,
        &successor,
        recurring,
        recovery,
        || rules.require_current(),
        |path| {
            write_frame(
                &mut *output,
                &Reply::Quarantined {
                    recovery: path.to_owned(),
                },
            )?;
            match read_frame(&mut *input)? {
                Decision::Publish => Ok(true),
                Decision::Abort => Ok(false),
                _ => Err(invalid("Invalid protected file publication decision")),
            }
        },
    )?;
    Ok(Some(match published {
        Some(published) => {
            let successor = published
                .receiver
                .map(|receiver| scoped_fingerprint(rules, receiver.fingerprint));
            if recurring && successor.is_none() {
                return Err(invalid("The confirmed successor fingerprint is missing"));
            }
            Reply::Published {
                payload,
                recovery: published.recovery,
                successor,
            }
        }
        None => Reply::Aborted,
    }))
}

fn scoped_fingerprint(rules: &Rules, receiver: [u8; 32]) -> [u8; 32] {
    let mut proof = Sha256::new_with_prefix(b"kortty-protected-file-replacement-v1\0");
    proof.update(rules.revision());
    proof.update(receiver);
    proof.finalize().into()
}
