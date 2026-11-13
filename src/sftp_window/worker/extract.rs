//! Review server-side archive extraction on the original authenticated SSH route.

use std::sync::Weak;

use kortty_server_archive::manifest::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES, Manifest};
use kortty_server_archive::protocol;
use kortty_server_archive::{Format, MAX_ARCHIVE_BYTES};

use super::sync::{Cancel, review_root};
use super::*;

pub(in crate::sftp_window) mod transport;

const UNKNOWN: &str = " · The server archive publication outcome is unknown. Check the reviewed destination before retrying.";

pub(in crate::sftp_window) fn supported_name(name: &[u8]) -> bool {
    std::str::from_utf8(name).is_ok_and(|name| component(name) && Format::detect(name).is_some())
}

pub(in crate::sftp_window) fn available(worker: &Worker) -> bool {
    worker.exec_opener().is_some()
}

pub(crate) struct Request {
    directory: Vec<u8>,
    selected: Entry,
    source: String,
    target: SshTarget,
    peer: HostKey,
    opener: ExecOpener,
    owner: Weak<Control>,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        directory: Vec<u8>,
        selected: Entry,
        target: SshTarget,
        worker: &Worker,
    ) -> io::Result<Self> {
        require_policy(&target)?;
        let opener = worker.exec_opener().ok_or_else(unavailable)?;
        let peer = worker
            .verified_host_key()
            .cloned()
            .ok_or_else(unavailable)?;
        let source = selected_source(&directory, &selected)?;
        Ok(Self {
            directory,
            selected,
            source,
            target,
            peer,
            opener,
            owner: Arc::downgrade(&worker.control),
        })
    }

    fn bound(&self, control: &Control) -> bool {
        std::ptr::eq(self.owner.as_ptr(), control)
            && control.host_key.as_ref() == Some(&self.peer)
            && self.opener.verified_host_key() == Some(&self.peer)
            && !self.opener.is_closed()
            && !control.is_closed()
    }
}

pub struct Plan {
    request: Request,
    review: protocol::Review,
    source_label: String,
    destination_label: String,
    used: AtomicBool,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerExtractPlan")
            .field("entries", &self.review.manifest.members.len())
            .field("expanded_bytes", &self.review.manifest.expanded_bytes)
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, target: &SshTarget) -> bool {
        self.request.target == *target
            && !self.used.load(Ordering::Acquire)
            && self.request.bound(&worker.control)
            && worker
                .exec_opener()
                .is_some_and(|opener| self.request.opener.same_connection(&opener))
            && require_policy(target).is_ok()
    }

    pub(in crate::sftp_window) fn source(&self) -> &str {
        &self.source_label
    }

    pub(in crate::sftp_window) fn destination(&self) -> &str {
        &self.destination_label
    }

    pub(in crate::sftp_window) fn manifest(&self) -> &Manifest {
        &self.review.manifest
    }
}

pub(super) fn prepare(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    let result = prepare_checked(client, request, cancel, control);
    cancel.finish();
    mark_disconnect(result, control)
}

fn prepare_checked(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    guard(&request, cancel, control)?;
    verify_source(client, &request)?;
    let reply = exchange(
        &request,
        &protocol::Request::Review {
            source: request.source.clone(),
        },
        cancel,
        control,
    )?;
    let review = match reply {
        protocol::Reply::Review { review } => review,
        protocol::Reply::Failed {
            message,
            outcome_unknown,
        } => return Err(helper_failure(&message, outcome_unknown)),
        protocol::Reply::Applied { .. } | protocol::Reply::CreationReview { .. } => {
            return Err(invalid_receipt().into());
        }
    };
    validate_review(&request.source, &review)?;
    guard(&request, cancel, control)?;
    verify_source(client, &request)?;
    require_absent(client, &review.destination)?;
    guard(&request, cancel, control)?;
    let source_label = review_root(&Location::Remote(request.source.as_bytes().to_vec()))?;
    let destination_label = review_root(&Location::Remote(review.destination.as_bytes().to_vec()))?;
    Ok(Plan {
        request,
        review,
        source_label,
        destination_label,
        used: AtomicBool::new(false),
    })
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    cancel: &Cancel,
    control: &Control,
) -> Result<Reply> {
    let result = apply_checked(client, plan, cancel, control);
    cancel.finish();
    mark_disconnect(result, control)
}

fn apply_checked(
    client: &mut Client,
    plan: &Plan,
    cancel: &Cancel,
    control: &Control,
) -> Result<Reply> {
    let request = &plan.request;
    guard(request, cancel, control)?;
    if plan.used.swap(true, Ordering::AcqRel) {
        return Err(
            io::Error::other("This server archive extraction review was already used").into(),
        );
    }
    verify_source(client, request)?;
    require_absent(client, &plan.review.destination)?;
    let reply = exchange(
        request,
        &protocol::Request::Apply {
            source: request.source.clone(),
            destination: plan.review.destination.clone(),
            token: plan.review.token.clone(),
        },
        cancel,
        control,
    )?;
    match reply {
        protocol::Reply::Applied { destination } if destination == plan.review.destination => {}
        protocol::Reply::Failed {
            message,
            outcome_unknown,
        } => return Err(helper_failure(&message, outcome_unknown)),
        _ => return Err(Failure::from(invalid_receipt()).context(UNKNOWN)),
    }

    let mut note = format!(
        "Extracted {} archive entries · {}",
        plan.manifest().members.len(),
        plan.destination()
    );
    let directory = if guard(request, cancel, control).is_ok() {
        refresh(client, &request.directory, control, &mut note)
    } else {
        None
    };
    Ok(Reply::Changed { directory, note })
}

fn exchange(
    request: &Request,
    message: &protocol::Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<protocol::Reply> {
    guard(request, cancel, control)?;
    let owner = request.owner.clone();
    let stopped = cancel.clone();
    let cancelled =
        move || stopped.stopped() || owner.upgrade().is_none_or(|owner| owner.is_closed());
    transport::exchange(&request.opener, message, cancelled, || {
        guard(request, cancel, control)
    })
    .map_err(Failure::from)
}

fn guard(request: &Request, cancel: &Cancel, control: &Control) -> io::Result<()> {
    if cancel.stopped() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Server archive extraction cancelled",
        ));
    }
    if !request.bound(control) {
        return Err(io::Error::other("The reviewed SSH connection changed"));
    }
    require_policy(&request.target)
}

fn require_policy(target: &SshTarget) -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() || !target.workflow_route_allowed(&policy) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Current policy does not allow this server archive extraction",
        ));
    }
    Ok(())
}

fn selected_source(directory: &[u8], entry: &Entry) -> io::Result<String> {
    let EntryName::Remote(name) = &entry.name else {
        return Err(invalid_source());
    };
    let directory_text = std::str::from_utf8(directory).map_err(|_| invalid_source())?;
    if entry.kind != EntryKind::File
        || !supported_name(name)
        || !valid_directory(directory_text)
        || !entry
            .size
            .is_some_and(|size| (1..=MAX_ARCHIVE_BYTES).contains(&size))
    {
        return Err(invalid_source());
    }
    let source = files::remote_join(directory, name);
    if source.len() > MAX_PATH_BYTES {
        return Err(invalid_source());
    }
    String::from_utf8(source).map_err(|_| invalid_source())
}

fn verify_source(client: &mut Client, request: &Request) -> Result<()> {
    let source = request.source.as_bytes();
    let attributes = client.lstat(source)?;
    let selected = &request.selected;
    if attributes.kind() != FileKind::File
        || attributes.size != selected.size
        || attributes.modified.is_none()
        || attributes.modified != selected.modified
        || attributes.uid.is_none()
        || attributes.uid != selected.owner
        || attributes.gid.is_none()
        || attributes.gid != selected.group
        || attributes.permissions != selected.mode
        || attributes.permissions.is_none_or(|mode| mode & 0o022 != 0)
        || client.canonicalize(source)? != source
    {
        return Err(io::Error::other("The selected server archive changed or is no longer a protected regular file. Refresh and review it again.").into());
    }
    Ok(())
}

fn validate_review(source: &str, review: &protocol::Review) -> io::Result<()> {
    if review.source != source
        || !eligible_destination(source, &review.destination)
        || !valid_token(&review.token)
    {
        return Err(invalid_receipt());
    }
    review.manifest.validate()
}

fn eligible_destination(source: &str, destination: &str) -> bool {
    let Some((parent, name)) = source.rsplit_once('/') else {
        return false;
    };
    let Some((destination_parent, leaf)) = destination.rsplit_once('/') else {
        return false;
    };
    let Some((_, stem)) = Format::detect(name) else {
        return false;
    };
    if parent != destination_parent
        || destination == source
        || destination.len() > MAX_PATH_BYTES
        || !component(leaf)
    {
        return false;
    }
    let stem = stem.trim();
    let stem = if component(stem) { stem } else { "extracted" };
    leaf == stem || (1..100).any(|index| leaf == format!("{stem} ({index})"))
}

fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_directory(directory: &str) -> bool {
    directory == "/"
        || directory.len() <= MAX_PATH_BYTES
            && directory
                .strip_prefix('/')
                .is_some_and(|parts| parts.split('/').all(component))
}

fn component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COMPONENT_BYTES
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

fn require_absent(client: &mut Client, destination: &str) -> Result<()> {
    if remote_existing(client, destination.as_bytes())?.is_some() {
        return Err(
            io::Error::other("The reviewed server archive destination already exists").into(),
        );
    }
    Ok(())
}

fn refresh(
    client: &mut Client,
    path: &[u8],
    control: &Control,
    note: &mut String,
) -> Option<Arc<Directory>> {
    let directory = Location::Remote(path.to_vec());
    match list(client, &directory) {
        Ok(result) if result.path == directory => Some(result),
        Ok(_) => {
            note.push_str(" · The selected server folder changed before refresh");
            None
        }
        Err(error) => {
            if error.disconnected {
                control.abort();
            }
            note.push_str(&format!(" · Refresh failed ({})", error.message));
            None
        }
    }
}

fn helper_failure(message: &str, outcome_unknown: bool) -> Failure {
    let message = files::display(message);
    let failure = Failure::from(io::Error::other(if message.is_empty() {
        "The server archive helper reported a failure".into()
    } else {
        message
    }));
    if outcome_unknown {
        failure.context(UNKNOWN)
    } else {
        failure
    }
}

fn mark_disconnect<T>(result: Result<T>, control: &Control) -> Result<T> {
    result.map_err(|mut error| {
        error.disconnected |= control.is_closed();
        error
    })
}

fn unavailable() -> io::Error {
    io::Error::other(
        "A verified live SSH command channel is required for server archive extraction",
    )
}

fn invalid_source() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Choose a regular server archive with a supported name and size",
    )
}

fn invalid_receipt() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The server archive helper returned an invalid extraction receipt",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_receipt_binds_exact_source_and_an_eligible_sibling() {
        let mut review = protocol::Review {
            source: "/folder/archive.TAR.GZ".into(),
            destination: "/folder/archive (99)".into(),
            token: "0123456789abcdef".repeat(4),
            manifest: Manifest {
                members: Vec::new(),
                expanded_bytes: 0,
            },
        };
        assert!(validate_review("/folder/archive.TAR.GZ", &review).is_ok());
        assert!(validate_review("/folder/other.TAR.GZ", &review).is_err());
        for destination in [
            "/other/archive",
            "/folder/archive (100)",
            "/folder/archive (01)",
            "/folder/archive (0)",
            "/folder/archive.TAR.GZ",
            "/folder//archive",
        ] {
            review.destination = destination.into();
            assert!(
                validate_review("/folder/archive.TAR.GZ", &review).is_err(),
                "{destination}"
            );
        }
        review.destination = "/folder/archive".into();
        review.token.pop();
        assert!(validate_review("/folder/archive.TAR.GZ", &review).is_err());
        assert!(eligible_destination("/..zip", "/extracted"));
    }

    #[test]
    fn extraction_source_requires_a_safe_utf8_archive_component() {
        for name in [b"a.zip".as_slice(), b"test.tar.bz2", b"book.7Z"] {
            assert!(supported_name(name));
        }
        for name in [
            b".zip".as_slice(),
            b"text.exe",
            b"x/y.zip",
            b"x\\y.zip",
            b"a\x00.zip",
            b"\xff.zip",
        ] {
            assert!(!supported_name(name));
        }
        assert!(valid_directory("/"));
        assert!(valid_directory("/user/private"));
        assert!(!valid_directory("/user/../private"));
        assert!(!valid_directory("/user//private"));
    }
}
