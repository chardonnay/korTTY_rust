//! Device-local approval for rsync transfers to new destinations.

use std::path::PathBuf;

use iced::widget::{button, column, row, text};
use iced::{Element, Length, Task};
use kortty_server_archive::rsync_tree::Inventory;
use serde::{Deserialize, Serialize};

use super::{ConnectionRef, Message, Page, RemotePath, SchedulerView};

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    pub(crate) program: PathBuf,
    pub(crate) local: PathBuf,
    pub(crate) remote: RemotePath,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) directory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<Approval>,
}

pub(crate) const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(unix)]
pub(crate) const MAX_TREE_ENTRIES: usize = 4_096;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryReview {
    pub(crate) source: RemotePath,
    pub(crate) destination: PathBuf,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadReview {
    pub(crate) source: PathBuf,
    pub(crate) destination: RemotePath,
    pub(crate) bytes: u64,
    pub(crate) digest: [u8; 32],
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryUploadReview {
    pub(crate) source: PathBuf,
    pub(crate) destination: RemotePath,
    pub(crate) inventory: Inventory,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub(crate) program: PathBuf,
    pub(crate) digest: [u8; 32],
    pub(crate) reference: ConnectionRef,
    pub(crate) pins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) upload: Option<UploadReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory: Option<Box<DirectoryReview>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory_upload: Option<Box<DirectoryUploadReview>>,
}

impl Approval {
    pub(super) fn fingerprint(&self) -> String {
        use std::fmt::Write as _;
        let mut result = String::with_capacity(64);
        for byte in self.digest {
            let _ = write!(result, "{byte:02x}");
        }
        result
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        super::remote::validate_local_path(&self.program)?;
        self.reference.validate()?;
        if !(1..=2).contains(&self.pins.len())
            || self
                .pins
                .iter()
                .any(|pin| pin.len() > 512 || pin.chars().any(char::is_control))
        {
            return Err("The rsync route approval is invalid.");
        }
        if let Some(upload) = &self.upload {
            super::remote::validate_local_path(&upload.source)?;
            if upload.source == self.program
                || !valid_remote(upload.destination.as_str())
                || upload.bytes > MAX_FILE_BYTES
            {
                return Err("The reviewed rsync upload is invalid.");
            }
        }
        if let Some(directory) = &self.directory {
            super::remote::validate_local_path(&directory.destination)?;
            if self.upload.is_some()
                || self.directory_upload.is_some()
                || directory.destination == self.program
                || !valid_remote(directory.source.as_str())
            {
                return Err("The reviewed rsync directory download is invalid.");
            }
        }
        if let Some(directory) = &self.directory_upload {
            super::remote::validate_local_path(&directory.source)?;
            if self.upload.is_some()
                || directory.source == self.program
                || !valid_remote(directory.destination.as_str())
                || !directory.inventory.valid()
            {
                return Err("The reviewed rsync directory upload is invalid.");
            }
        }
        Ok(())
    }
}

impl Transfer {
    pub(crate) fn validate(&self, upload: bool) -> Result<(), &'static str> {
        super::remote::validate_local_path(&self.program)?;
        super::remote::validate_local_path(&self.local)?;
        if self.program == self.local {
            return Err("Choose a transfer path separate from the rsync executable.");
        }
        if !valid_remote(self.remote.as_str()) {
            return Err(
                "Use an absolute server path containing letters, digits, slash, dot, underscore, or hyphen.",
            );
        }
        if let Some(approval) = &self.approval {
            approval.validate()?;
            if (upload && !self.directory) != approval.upload.is_some()
                || (!upload && self.directory) != approval.directory.is_some()
                || (upload && self.directory) != approval.directory_upload.is_some()
                || approval.upload.as_ref().is_some_and(|review| {
                    review.source != self.local || review.destination != self.remote
                })
                || approval.directory.as_ref().is_some_and(|review| {
                    review.source != self.remote || review.destination != self.local
                })
                || approval.directory_upload.as_ref().is_some_and(|review| {
                    review.source != self.local || review.destination != self.remote
                })
            {
                return Err("Review the exact rsync transfer paths again.");
            }
        }
        Ok(())
    }
}

pub(crate) fn valid_remote(path: &str) -> bool {
    path.len() <= 1024
        && path.starts_with('/')
        && !path.ends_with('/')
        && path
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

pub(super) struct ReviewRequest {
    pub(super) program: PathBuf,
    pub(super) reference: ConnectionRef,
    pub(super) upload: Option<(PathBuf, RemotePath)>,
    pub(super) directory: Option<DirectoryReview>,
    pub(super) directory_upload: Option<(PathBuf, RemotePath)>,
}

impl ReviewRequest {
    pub(super) fn matches(&self, approval: &Approval) -> bool {
        self.program == approval.program
            && self.reference == approval.reference
            && self.directory.as_ref() == approval.directory.as_deref()
            && self
                .directory_upload
                .as_ref()
                .map(|(source, destination)| (source, destination))
                == approval
                    .directory_upload
                    .as_ref()
                    .map(|review| (&review.source, &review.destination))
            && match (&self.upload, &approval.upload) {
                (None, None) => true,
                (Some((source, destination)), Some(review)) => {
                    &review.source == source && &review.destination == destination
                }
                _ => false,
            }
    }
}

fn review(request: ReviewRequest) -> Result<Approval, String> {
    super::remote::validate_local_path(&request.program)?;
    let trust = crate::ssh_connection::trust_store().map_err(|error| error.to_string())?;
    let pins = super::sync::pins(&request.reference, &trust).map_err(|error| error.to_string())?;
    let digest = crate::job_executor::rsync::program_digest(&request.program)
        .map_err(|error| error.to_string())?;
    let upload = if let Some((source, destination)) = request.upload {
        super::remote::validate_local_path(&source)?;
        if source == request.program || !valid_remote(destination.as_str()) {
            return Err("Choose a regular local source and an absolute server file path.".into());
        }
        let (bytes, digest) = crate::job_executor::rsync::upload_digest(&source)
            .map_err(|error| error.to_string())?;
        Some(UploadReview {
            source,
            destination,
            bytes,
            digest,
        })
    } else {
        None
    };
    let directory_upload = request
        .directory_upload
        .map(|(source, destination)| {
            if source == request.program || !valid_remote(destination.as_str()) {
                return Err("Choose one local directory and a new absolute server path.".to_owned());
            }
            let inventory = crate::job_executor::rsync::upload_tree_inventory(&source)
                .map_err(|error| error.to_string())?;
            Ok(Box::new(DirectoryUploadReview {
                source,
                destination,
                inventory,
            }))
        })
        .transpose()?;
    let approval = Approval {
        program: request.program,
        digest,
        reference: request.reference,
        pins: super::sync::fingerprints(&pins),
        upload,
        directory: request.directory.map(Box::new),
        directory_upload,
    };
    approval.validate()?;
    Ok(approval)
}

#[derive(Default)]
pub(super) struct Editor {
    pending: Option<String>,
    approval: Option<Approval>,
}

impl Editor {
    pub(super) fn begin(&mut self, ticket: String) {
        *self = Self {
            pending: Some(ticket),
            approval: None,
        };
    }

    pub(super) fn complete(
        &mut self,
        ticket: &str,
        result: Result<Approval, String>,
    ) -> Option<String> {
        if self.pending.as_deref() != Some(ticket) {
            return None;
        }
        self.pending = None;
        match result {
            Ok(approval) => {
                self.approval = Some(approval);
                Some("Review the executable, verified SSH route and transfer details.".into())
            }
            Err(error) => Some(error),
        }
    }

    pub(super) fn take(&mut self) -> Option<Approval> {
        self.approval.take()
    }

    pub(super) fn view(&self) -> Option<Element<'_, Message>> {
        if self.pending.is_some() {
            return Some(
                row![
                    text("Checking the executable and saved SSH route…").width(Length::Fill),
                    button("Cancel review").on_press(Message::DiscardRsyncProgram),
                ]
                .spacing(6)
                .into(),
            );
        }
        let approval = self.approval.as_ref()?;
        let pins = approval
            .pins
            .iter()
            .fold(column![].spacing(4), |items, pin| {
                items.push(text(pin).size(12))
            });
        Some(
            column![
                text("Review rsync transfer").size(17),
                text(approval.program.display().to_string()),
                text(format!("SHA-256 · {}", approval.fingerprint())).size(12),
                pins,
                approval.upload.as_ref().map(|upload| {
                    let digest = upload
                        .digest
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    column![
                        text(format!("Local source · {}", upload.source.display())),
                        text(format!(
                            "New server destination · {}",
                            upload.destination.as_str()
                        )),
                        text(format!("{} bytes · SHA-256 {digest}", upload.bytes)).size(12),
                    ]
                    .spacing(4)
                }),
                approval.directory.as_ref().map(|directory| column![
                    text(format!("Server directory · {}", directory.source.as_str())),
                    text(format!("New local folder · {}", directory.destination.display())),
                    text("Each run checks the complete tree, up to 4,096 entries and 256 MiB. A destination that exists blocks the run.").size(12),
                ].spacing(4)),
                approval.directory_upload.as_ref().map(|directory| {
                    let inventory = directory.inventory;
                    let digest = inventory.digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                    column![
                        text(format!("Local directory · {}", directory.source.display())),
                        text(format!("New server folder · {}", directory.destination.as_str())),
                        text(format!("{} files · {} folders · {} bytes", inventory.files, inventory.entries - inventory.files, inventory.bytes)).size(12),
                        text(format!("Inventory SHA-256 · {digest}")).size(12),
                    ].spacing(4)
                }),
                row![
                    button("Discard review").on_press(Message::DiscardRsyncProgram),
                    button("Approve").on_press(Message::ApproveRsyncProgram),
                ]
                .spacing(6),
            ]
            .spacing(6)
            .into(),
        )
    }
}

impl SchedulerView {
    pub(crate) fn begin_rsync_review(&mut self) -> Task<Message> {
        if !cfg!(unix) || self.page != Page::Action || self.confirm.is_some() {
            return Task::none();
        }
        let Some(request) = self.draft.rsync_candidate() else {
            self.report_status(
                "Choose one saved SSH connection and valid absolute rsync and transfer paths."
                    .into(),
            );
            return Task::none();
        };
        let Ok(ticket) = crate::private_files::nonce() else {
            self.report_status("Could not allocate the rsync review.".into());
            return Task::none();
        };
        self.rsync_review.begin(ticket.clone());
        self.status = None;
        let (sender, receiver) = async_channel::bounded(1);
        let _ = std::thread::Builder::new()
            .name("kortty-rsync-review".into())
            .spawn(move || {
                let _ = sender.send_blocking(review(request));
            });
        Task::future(async move {
            let result = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("Could not check the rsync executable.".into()));
            Message::RsyncProgramReviewed(ticket, result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_upload_approval_cannot_be_reused_for_other_paths_or_a_download() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let local = root.join("source");
        let remote = RemotePath::try_from("/server/destination".to_owned()).unwrap();
        let reference: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "user"}
        }))
        .unwrap();
        let mut transfer = Transfer {
            program: program.clone(),
            local: local.clone(),
            remote: remote.clone(),
            directory: false,
            approval: Some(Approval {
                program,
                digest: [1; 32],
                reference,
                pins: vec!["server.example SHA256:fixture".into()],
                upload: Some(UploadReview {
                    source: local,
                    destination: remote,
                    bytes: 4,
                    digest: [2; 32],
                }),
                directory: None,
                directory_upload: None,
            }),
        };
        assert!(transfer.validate(true).is_ok());
        assert!(transfer.validate(false).is_err());
        transfer.local = root.join("other-source");
        assert!(transfer.validate(true).is_err());
    }

    #[test]
    fn rsync_directory_requires_an_approval_for_the_exact_roots() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let local = root.join("new-folder");
        let remote = RemotePath::try_from("/server/source".to_owned()).unwrap();
        let program = root.join("rsync");
        let reference: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 1,
            "target": {"host": "server.example", "port": 22, "username": "user"}
        }))
        .unwrap();
        let digest = [1; 32];
        let mut transfer: Transfer = serde_json::from_value(serde_json::json!({
            "program": program, "local": local, "remote": remote,
            "approval": {"program": program, "digest": digest, "reference": reference,
                "pins": ["server.example SHA256:fixture"]}
        }))
        .unwrap();
        assert!(transfer.validate(false).is_ok());
        assert!(
            serde_json::to_value(&transfer)
                .unwrap()
                .get("directory")
                .is_none()
        );
        transfer.directory = true;
        assert!(transfer.validate(false).is_err());
        transfer.approval.as_mut().unwrap().directory = Some(Box::new(DirectoryReview {
            source: remote,
            destination: local,
        }));
        assert!(transfer.validate(false).is_ok());
        assert!(transfer.validate(true).is_err());
        transfer.local = root.join("other-folder");
        assert!(transfer.validate(false).is_err());
    }
}
