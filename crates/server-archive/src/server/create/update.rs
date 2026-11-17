//! Checked publication of successive archive outputs on Linux.

use std::time::Duration;

use kortty_server_archive::protocol::{
    UpdateDecision, UpdateReply, UpdateRequest, read_update_decision, read_update_password,
    read_update_request, write_update_reply,
};

use super::super::privileged_archive::{Rules, prepare_copy, stream_archive_copy};
use super::super::rsync_tree::{
    CheckedFile, ExistingFile, ExistingRecovery, RECEIVED_FILE, UploadStage, VacantFile,
};
use super::*;

const DEADLINE: Duration = Duration::from_secs(30 * 60);

pub(crate) fn serve(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    std::thread::Builder::new()
        .name("kortty-archive-update-deadline".into())
        .spawn(|| {
            std::thread::sleep(DEADLINE);
            std::process::exit(124);
        })?;
    let mut recovery = ExistingRecovery::default();
    let reply = (|| {
        if getuid() != geteuid() || getgid() != getegid() || geteuid().is_root() {
            return Err(invalid(
                "Reviewed archive updates require an ordinary login",
            ));
        }
        umask(Mode::from_bits_truncate(0o077));
        let policy = PathPolicy::default();
        match read_update_request(&mut input)? {
            UpdateRequest::Review { spec } => review(spec, policy).map(Reviewed::reply),
            UpdateRequest::Prepare { spec, token } => transfer(
                &spec,
                &token,
                None,
                policy,
                &mut input,
                &mut output,
                &mut recovery,
            )
            .map(|publication| publication.map_or(UpdateReply::Aborted, Published::reply)),
        }
    })()
    .unwrap_or_else(|error| failed(error, recovery));
    write_update_reply(output, &reply)
}

pub(in crate::server) fn serve_privileged(
    mut input: impl Read,
    mut output: impl Write,
    rules: &Rules,
    on_prepare: impl FnOnce(),
) -> io::Result<()> {
    let mut recovery = ExistingRecovery::default();
    let transaction = (|| {
        let UpdateRequest::Review { spec } = read_update_request(&mut input)? else {
            return Err(invalid(
                "A privileged archive update must start with a review",
            ));
        };
        let policy = PathPolicy::privileged(rules);
        let mut reviewed = review(spec.clone(), policy)?;
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let commitment = hex::encode(nonce);
        let token = std::mem::replace(&mut reviewed.review.token, commitment.clone());
        let receiver = reviewed.receiver;
        write_update_reply(&mut output, &reviewed.reply())?;
        match read_update_request(&mut input) {
            Ok(UpdateRequest::Prepare {
                spec: requested,
                token: proof,
            }) if requested == spec && proof == commitment => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            _ => {
                return Err(invalid(
                    "The privileged archive preparation does not match this review",
                ));
            }
        }
        rules.require_current()?;
        on_prepare();
        transfer(
            &spec,
            &token,
            Some(receiver),
            policy,
            &mut input,
            &mut output,
            &mut recovery,
        )
    })();
    let mut published = match transaction {
        Ok(Some(published)) => published,
        Ok(None) => return write_update_reply(output, &UpdateReply::Aborted),
        Err(error) => return write_update_reply(output, &failed(error, recovery)),
    };
    let destination = published.destination.clone();
    let payload = published.payload;
    let mut copy = published.copy.take();
    write_update_reply(&mut output, &published.reply())?;
    let Some(file) = &mut copy else {
        return Ok(());
    };
    if !matches!(decision(input), Ok(UpdateDecision::FetchCopy))
        || prepare_exact_copy(rules, &destination, file, payload).is_err()
    {
        return write_update_reply(output, &UpdateReply::CopyFailed);
    }
    write_update_reply(&mut output, &UpdateReply::CopyReady { payload })?;
    stream_archive_copy(&mut output, rules, file, payload)
}

fn prepare_exact_copy(
    rules: &Rules,
    destination: &str,
    file: &mut File,
    payload: CreatedPayload,
) -> io::Result<()> {
    prepare_copy(rules, destination, file, payload)?;
    let stamp = Stamp::of(&file.metadata()?);
    if summarize_payload(file)? != payload || Stamp::of(&file.metadata()?) != stamp {
        return Err(invalid("The published privileged archive bytes changed"));
    }
    prepare_copy(rules, destination, file, payload)
}

fn failed(error: io::Error, recovery: ExistingRecovery) -> UpdateReply {
    UpdateReply::Failed {
        message: Failure::from(error).message,
        outcome_unknown: recovery.uncertain,
        recovery: recovery.path,
    }
}

struct Reviewed {
    review: CreateReview,
    existing: Option<CreatedPayload>,
    receiver: [u8; 32],
}

impl Reviewed {
    fn reply(self) -> UpdateReply {
        UpdateReply::Reviewed {
            review: self.review,
            existing: self.existing,
            receiver: self.receiver,
        }
    }
}

struct Published {
    destination: String,
    payload: CreatedPayload,
    recovery: Option<String>,
    receiver: [u8; 32],
    copy: Option<File>,
}

impl Published {
    fn reply(self) -> UpdateReply {
        UpdateReply::Published {
            destination: self.destination,
            payload: self.payload,
            recovery: self.recovery,
            receiver: self.receiver,
        }
    }
}

enum Receiver<'a> {
    Vacant(Box<VacantFile<'a>>),
    Existing(Box<ExistingFile<'a>>),
}

impl<'a> Receiver<'a> {
    fn open(request: &Creation<'a>) -> io::Result<Self> {
        let parent = Directory::from_fd(request.parent.fd.try_clone()?.into())?;
        let destination = &request.spec.destination;
        let vacant = absent(&parent.fd, request.name)?;
        require_scope(request, !vacant)?;
        if vacant {
            Ok(Self::Vacant(Box::new(VacantFile::open_at(
                destination,
                parent,
            )?)))
        } else {
            let target = ExistingFile::open_archive_at(destination, parent)?;
            if request.policy.rules.is_some()
                && target.original_archive()?.0.metadata()?.mode() & 0o7777 != 0o600
            {
                return Err(invalid(
                    "Privileged archive replacement requires a root-private receiver with mode 0600",
                ));
            }
            Ok(Self::Existing(Box::new(target)))
        }
    }

    fn parent(&self) -> &Directory {
        match self {
            Self::Vacant(target) => target.parent(),
            Self::Existing(target) => target.parent(),
        }
    }

    fn fingerprint(&self) -> [u8; 32] {
        match self {
            Self::Vacant(target) => target.fingerprint(),
            Self::Existing(target) => target.review().fingerprint,
        }
    }

    fn original(&self) -> io::Result<Option<(File, CreatedPayload)>> {
        match self {
            Self::Vacant(target) => {
                target.require_vacant()?;
                Ok(None)
            }
            Self::Existing(target) => target.original_archive().map(Some),
        }
    }

    fn require_current(&self) -> io::Result<()> {
        match self {
            Self::Vacant(target) => target.require_vacant(),
            Self::Existing(target) => target.require_current(),
        }
    }
}

fn open<'a>(spec: &'a CreateSpec, policy: PathPolicy<'a>) -> io::Result<Creation<'a>> {
    let request = Creation::open(spec, policy)?;
    if spec.output.is_some_and(|metadata| {
        metadata.uid.is_some_and(|uid| uid != geteuid().as_raw())
            || metadata.mode.is_some_and(|mode| mode & 0o422 != 0o400)
    }) {
        return Err(invalid(
            "Archive updates require login ownership, owner read access, and no group or public writes",
        ));
    }
    Ok(request)
}

fn require_scope(request: &Creation<'_>, replacing: bool) -> io::Result<()> {
    if let Some(rules) = request.policy.rules {
        rules.require_current()?;
        let current = rules.update_parent(request.parent_path, replacing)?;
        let mount_id = super::super::draft_replace::mount_id;
        if !same_node(&request.parent.stat, &current.stat)
            || mount_id(&request.parent.fd)? != mount_id(&current.fd)?
        {
            return Err(invalid("The privileged archive destination folder changed"));
        }
    }
    Ok(())
}

fn review(spec: CreateSpec, policy: PathPolicy<'_>) -> io::Result<Reviewed> {
    let request = open(&spec, policy)?;
    let target = Receiver::open(&request)?;
    let receiver = target.fingerprint();
    let existing = target.original()?.map(|(_, payload)| payload);
    let inventory = request.scan(|_, input| {
        io::copy(input, &mut io::sink())?;
        Ok(())
    })?;
    require_scope(&request, existing.is_some())?;
    target.require_current()?;
    let token = bind_review(&inventory.token, receiver, policy);
    Ok(Reviewed {
        review: CreateReview {
            spec,
            token,
            manifest: inventory.manifest,
            excluded: inventory.excluded,
        },
        existing,
        receiver,
    })
}

fn bind_review(inventory: &str, receiver: [u8; 32], policy: PathPolicy<'_>) -> String {
    let mut hash = Sha256::new();
    hash.update(b"kortty server archive update v1\0");
    hash.update(inventory.as_bytes());
    hash.update(receiver);
    if let Some(rules) = policy.rules {
        rules.hash_update_rules(&mut hash);
    }
    hex::encode(hash.finalize())
}

fn transfer(
    spec: &CreateSpec,
    token: &str,
    reviewed_receiver: Option<[u8; 32]>,
    policy: PathPolicy<'_>,
    mut input: impl Read,
    mut output: impl Write,
    recovery: &mut ExistingRecovery,
) -> io::Result<Option<Published>> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("Invalid reviewed archive update token"));
    }
    let request = open(spec, policy)?;
    let target = Receiver::open(&request)?;
    if reviewed_receiver.is_some_and(|receiver| target.fingerprint() != receiver) {
        return Err(invalid("The reviewed archive receiver changed"));
    }
    let password = if spec.format == CreateFormat::ZipPassword {
        write_update_reply(&mut output, &UpdateReply::PasswordRequired)?;
        Some(read_update_password(&mut input)?)
    } else {
        None
    };
    let mut stage = UploadStage::create_checked(target.parent())?;
    let (candidate, payload, copy) = stage_archive(
        &request,
        &target,
        &stage,
        token,
        password.as_deref().map(Vec::as_slice),
    )?;
    drop(password);
    require_scope(&request, matches!(&target, Receiver::Existing(_)))?;
    target.require_current()?;
    write_update_reply(&mut output, &UpdateReply::Prepared { payload })?;
    let (receiver, displaced) = match (&target, decision(&mut input)?) {
        (_, UpdateDecision::Abort) => {
            stage.discard()?;
            return Ok(None);
        }
        (Receiver::Vacant(target), UpdateDecision::Publish) => {
            let successor = target.publish(&stage, &candidate, &mut recovery.uncertain, || {
                require_scope(&request, false)
            })?;
            stage.discard()?;
            (successor, None)
        }
        (Receiver::Existing(target), UpdateDecision::Quarantine) => {
            let publication = target.commit_checked(
                &mut stage,
                &candidate,
                true,
                recovery,
                || require_scope(&request, true),
                |path| {
                    write_update_reply(
                        &mut output,
                        &UpdateReply::Quarantined {
                            recovery: path.into(),
                        },
                    )?;
                    match decision(&mut input)? {
                        UpdateDecision::Publish => Ok(true),
                        UpdateDecision::Abort => Ok(false),
                        UpdateDecision::Quarantine | UpdateDecision::FetchCopy => {
                            Err(invalid("Unexpected archive publication decision"))
                        }
                    }
                },
            )?;
            let Some(publication) = publication else {
                stage.discard()?;
                return Ok(None);
            };
            (
                publication
                    .receiver
                    .ok_or_else(|| invalid("The published archive receipt is unavailable"))?,
                Some(publication.recovery),
            )
        }
        _ => {
            return Err(invalid(
                "The archive publication decision does not match the reviewed receiver",
            ));
        }
    };
    Ok(Some(Published {
        destination: spec.destination.clone(),
        payload,
        recovery: displaced,
        receiver: receiver.fingerprint,
        copy,
    }))
}

fn decision(input: impl Read) -> io::Result<UpdateDecision> {
    match read_update_decision(input) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(UpdateDecision::Abort),
        other => other,
    }
}

fn stage_archive(
    request: &Creation<'_>,
    target: &Receiver<'_>,
    stage: &UploadStage<'_>,
    token: &str,
    password: Option<&[u8]>,
) -> io::Result<(CheckedFile, CreatedPayload, Option<File>)> {
    let mut output = stage_file(stage)?;
    let previous = match request.spec.format {
        CreateFormat::Zip | CreateFormat::ZipPassword => target.original()?.map(|(file, _)| file),
        _ => None,
    };
    if let Some(previous) = previous {
        let mut fresh_stage = UploadStage::create_checked(target.parent())?;
        let mut fresh = stage_file(&fresh_stage)?;
        encode_reviewed(request, target, &mut fresh, token, password)?;
        let mut capped = ArchiveOutput::new(&mut output, MAX_ARCHIVE_BYTES);
        super::zip_update::rebuild(previous, fresh, &mut capped, password, request.spec.level)?;
        fresh_stage.discard()?;
    } else {
        encode_reviewed(request, target, &mut output, token, password)?;
    }
    target.require_current()?;
    let payload = summarize_payload(&mut output)?;
    if let Some(metadata) = request.spec.output {
        apply_output_metadata(&output, metadata)?;
    } else if let Receiver::Existing(target) = target {
        target.retain_archive_metadata(&output)?;
    }
    let copy = (request.policy.rules.is_some() && request.spec.report_payload)
        .then(|| output.try_clone())
        .transpose()?;
    let candidate = CheckedFile::generated_archive(stage, output, payload)?;
    Ok((candidate, payload, copy))
}

fn stage_file(stage: &UploadStage<'_>) -> io::Result<File> {
    let file = File::from(openat(
        &stage.payload.fd,
        RECEIVED_FILE,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        PRIVATE_FILE,
    )?);
    super::super::privileged_archive::prepare_stage(&file)?;
    make_private(&file, PRIVATE_FILE)?;
    Ok(file)
}

fn encode_reviewed(
    request: &Creation<'_>,
    target: &Receiver<'_>,
    file: &mut File,
    token: &str,
    password: Option<&[u8]>,
) -> io::Result<()> {
    let mut output = ArchiveOutput::new(file, MAX_ARCHIVE_BYTES);
    let mut encoder = Encoder::new(request.spec, &mut output, password)?;
    let inventory = request.scan(|member, reader| encoder.write(member, reader))?;
    if bind_review(&inventory.token, target.fingerprint(), request.policy) != token {
        return Err(invalid("The reviewed archive inputs or receiver changed"));
    }
    encoder.finish()
}
