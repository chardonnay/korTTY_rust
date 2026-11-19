//! Fixed privileged rsync review and publication commands on Linux.

use std::io::{self, Read, Write};
use std::time::Duration;

use kortty_server_archive::privileged_rsync::{
    Operation, Reply, Request, read_request, write_reply,
};
use rustix::fs::Mode;
use rustix::process::umask;

mod download;
pub(crate) mod file_replace;
mod handoff;
mod policy;
pub(crate) mod tree_merge;
mod upload;

use policy::Rules;

const SOURCE_REVIEW_POLICY_INTERVAL: usize = 32;

#[derive(Default)]
struct Recovery {
    outcome_unknown: bool,
    recovery: Option<String>,
}

pub(crate) fn serve() -> io::Result<()> {
    let mut output = io::stdout().lock();
    let mut recovery = Recovery::default();
    let reply = (|| {
        umask(Mode::from_bits_truncate(0o077));
        let rules = Rules::load()?;
        std::thread::Builder::new()
            .name("kortty-rsync-helper-deadline".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(30 * 60));
                std::process::exit(124);
            })?;
        transaction(&rules, &mut io::stdin().lock(), &mut output, &mut recovery)
    })()
    .unwrap_or_else(|error| Reply::Failed {
        message: super::Failure::from(error).message,
        outcome_unknown: recovery.outcome_unknown,
        recovery: recovery.recovery,
    });
    write_reply(output, &reply)
}

fn transaction(
    rules: &Rules,
    input: &mut impl Read,
    output: &mut impl Write,
    recovery: &mut Recovery,
) -> io::Result<Reply> {
    let request = read_request(&mut *input)?;
    rules.require_current()?;
    match request {
        Request::Review { spec } => {
            let review = match spec.operation {
                Operation::Upload { .. } => upload::review(rules, spec)?,
                Operation::Download { .. } => download::review(rules, spec)?,
            };
            rules.require_current()?;
            if !review.valid() {
                return Err(super::invalid("The privileged rsync review is invalid"));
            }
            Ok(Reply::Reviewed { review })
        }
        Request::ReviewRecurringFileUpload { spec }
        | Request::ReviewRecurringTreeUpload { spec } => {
            let review = upload::review_recurring(rules, spec)?;
            rules.require_current()?;
            if !review.valid() {
                return Err(super::invalid(
                    "The recurring privileged rsync review is invalid",
                ));
            }
            Ok(Reply::Reviewed { review })
        }
        Request::ReviewSource { spec } => {
            stream_manifest(rules, output, download::review_manifest(rules, spec)?)
        }
        Request::ReviewSourceLimited { approved, limits } => {
            if approved.policy != rules.revision() || approved.handoff != rules.handoff()?.0 {
                return Err(super::invalid("The privileged rsync source scope changed"));
            }
            stream_manifest(
                rules,
                output,
                download::review_limited(rules, approved.spec, limits)?,
            )
        }
        Request::Prepare { review, incoming } if review.policy == rules.revision() => {
            match review.spec.operation {
                Operation::Upload { .. } => upload::apply(
                    rules,
                    review,
                    incoming.ok_or_else(|| super::invalid("Missing approved rsync input"))?,
                    input,
                    output,
                    recovery,
                    false,
                ),
                Operation::Download { .. } => download::apply(rules, review, input, output),
            }
        }
        Request::PrepareRecurring { review, incoming }
        | Request::PrepareFirstRecurringFileUpload { review, incoming }
        | Request::PrepareFirstRecurringTreeUpload { review, incoming }
            if review.policy == rules.revision() =>
        {
            upload::apply(rules, review, incoming, input, output, recovery, true)
        }
        Request::Prepare { .. }
        | Request::PrepareRecurring { .. }
        | Request::PrepareFirstRecurringFileUpload { .. }
        | Request::PrepareFirstRecurringTreeUpload { .. } => {
            Err(super::invalid("The privileged rsync policy changed"))
        }
    }
}

fn stream_manifest(
    rules: &Rules,
    output: &mut impl Write,
    (review, entries): (
        kortty_server_archive::privileged_rsync::Review,
        Vec<kortty_server_archive::rsync_tree::projection::Entry>,
    ),
) -> io::Result<Reply> {
    let inventory = review
        .inventory
        .ok_or_else(|| super::invalid("Missing privileged rsync source inventory"))?;
    rules.require_current()?;
    write_reply(&mut *output, &Reply::Reviewed { review })?;
    for (index, entry) in entries.into_iter().enumerate() {
        // Bound disclosure between policy checks even if a reader stalls the stream.
        if index.is_multiple_of(SOURCE_REVIEW_POLICY_INTERVAL) {
            rules.require_current()?;
        }
        write_reply(&mut *output, &Reply::SourceEntry { entry })?;
    }
    rules.require_current()?;
    Ok(Reply::SourceComplete { inventory })
}
