use std::path::{Path, PathBuf};

use iced::widget::{column, text};
use kortty_server_archive::privileged_rsync::{Operation, Review as PrivilegedReview};
use kortty_server_archive::rsync_tree::projection::{
    self, CollisionKind, Placement, Projection, Source,
};
use kortty_server_archive::rsync_tree::{
    DeleteScope, ExistingTree, Inventory, RecurringReceiver, valid_name,
};
use serde::{Deserialize, Serialize};

use crate::job_executor::rsync::{LocalExistingSnapshot, VacantLocalTreeSnapshot};

use super::{
    ConnectionRef, Elevation, MAX_FILE_BYTES, MAX_SOURCES, MAX_TREE_ENTRIES, Message, RemotePath,
    Transfer, elevation::existing_snapshot, local_scope_path, valid_remote,
};

const BAD_SOURCES: &str = "Choose one to sixteen literal absolute rsync sources.";
const BAD_REVIEW: &str = "The reviewed mixed rsync paths, inventory or deletion scope changed.";
const COLLISION_SAMPLES: usize = 8;

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum MixedDestination {
    Server(RemotePath),
    Local(PathBuf),
}

#[derive(Clone)]
pub(crate) struct MixedReviewTarget {
    pub(crate) reference: ConnectionRef,
    pub(crate) sources: Vec<String>,
    pub(crate) destination: MixedDestination,
    pub(crate) delete: bool,
    pub(crate) elevation: Elevation,
    pub(crate) review_vacant: bool,
}

impl MixedReviewTarget {
    pub(crate) fn matches(&self, review: &MixedReview) -> bool {
        review
            .receiver
            .matches(&self.destination, self.review_vacant)
            && review.matches_server_vacancy(self.review_vacant, self.elevation)
            && review.valid()
            && (self.elevation == Elevation::Passwordless) == review.privileged.is_some()
            && review
                .plan
                .matches(&self.sources, &self.destination, self.delete)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MixedReceiver {
    Server {
        destination: RemotePath,
        snapshot: Option<ExistingTree>,
    },
    Local {
        destination: PathBuf,
        snapshot: Option<LocalExistingSnapshot>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vacant: Option<Box<VacantLocalTreeSnapshot>>,
    },
}

impl MixedReceiver {
    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Server {
                destination,
                snapshot,
            } => {
                valid_remote(destination.as_str())
                    && snapshot
                        .as_ref()
                        .is_none_or(|snapshot| snapshot.inventory.valid())
            }
            Self::Local {
                destination,
                snapshot,
                vacant,
            } => {
                super::super::remote::validate_local_path(destination).is_ok()
                    && snapshot.as_ref().is_none_or(LocalExistingSnapshot::valid)
                    && vacant.as_deref().is_none_or(|snapshot| {
                        snapshot.valid() && snapshot.first_missing(destination).is_some()
                    })
                    && (snapshot.is_none() || vacant.is_none())
            }
        }
    }

    fn matches(&self, destination: &MixedDestination, review_vacant: bool) -> bool {
        match (self, destination) {
            (Self::Server { destination, .. }, MixedDestination::Server(path)) => {
                destination == path
            }
            (
                Self::Local {
                    destination,
                    snapshot,
                    vacant,
                },
                MixedDestination::Local(path),
            ) => destination == path && vacant.is_some() == (review_vacant && snapshot.is_none()),
            _ => false,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedReview {
    pub(crate) plan: MixedPlan,
    pub(crate) receiver: MixedReceiver,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) privileged: Option<MixedPrivilegedReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ordinary_receiver: Option<RecurringReceiver>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MixedPrivilegedReview {
    Upload(Box<PrivilegedReview>),
    Download(Vec<PrivilegedReview>),
}

impl MixedPrivilegedReview {
    fn matches(&self, plan: &MixedPlan, receiver: &MixedReceiver) -> bool {
        match (self, receiver) {
            (
                Self::Upload(review),
                MixedReceiver::Server {
                    destination,
                    snapshot,
                },
            ) => {
                let replaces = snapshot.is_some() || review.first_tree;
                review.valid()
                    && review.spec.path == destination.as_str()
                    && review.spec.operation
                        == (Operation::Upload {
                            directory: true,
                            replace: replaces,
                            delete: if replaces {
                                plan.delete.clone()
                            } else {
                                DeleteScope::Retain
                            },
                        })
                    && existing_snapshot(review) == *snapshot
            }
            (Self::Download(reviews), MixedReceiver::Local { .. }) => {
                let Some(first) = reviews.first() else {
                    return false;
                };
                reviews.len() == plan.sources.len()
                    && reviews.iter().zip(&plan.sources).all(|(review, source)| {
                        review.valid()
                            && review.policy == first.policy
                            && review.handoff == first.handoff
                            && review.spec.path == bare_source(&source.raw, false)
                            && review.spec.operation
                                == (Operation::Download {
                                    directory: !matches!(source.placement, Placement::File(_)),
                                })
                            && review.inventory.is_some_and(|raw| {
                                if matches!(source.placement, Placement::Contents) {
                                    return raw == source.inventory;
                                }
                                raw.bytes == source.inventory.bytes
                                    && raw.files == source.inventory.files
                                    && raw.entries.checked_add(u32::from(matches!(
                                        source.placement,
                                        Placement::Named(_)
                                    ))) == Some(source.inventory.entries)
                            })
                    })
            }
            _ => false,
        }
    }
}

impl MixedReview {
    fn first_server_tree_upload(&self) -> bool {
        matches!(self.privileged.as_ref(), Some(MixedPrivilegedReview::Upload(proof)) if proof.first_tree)
    }

    pub(super) fn matches_server_vacancy(&self, allow_vacant: bool, elevation: Elevation) -> bool {
        match &self.receiver {
            MixedReceiver::Server { .. } if allow_vacant && elevation == Elevation::Ordinary => {
                self.ordinary_receiver.is_some()
            }
            MixedReceiver::Server { snapshot, .. } if allow_vacant => {
                self.ordinary_receiver.is_none()
                    && (snapshot.is_some() || self.first_server_tree_upload())
            }
            MixedReceiver::Server { .. } => {
                !self.first_server_tree_upload() && self.ordinary_receiver.is_none()
            }
            MixedReceiver::Local { .. } => self.ordinary_receiver.is_none(),
        }
    }

    pub(super) fn valid(&self) -> bool {
        self.plan.valid()
            && self.receiver.valid()
            && self.ordinary_receiver.is_none_or(|proof| {
                proof.valid(true)
                    && self.privileged.is_none()
                    && match (&self.receiver, proof) {
                        (
                            MixedReceiver::Server { snapshot: None, .. },
                            RecurringReceiver::Vacant { .. },
                        ) => true,
                        (
                            MixedReceiver::Server {
                                snapshot: Some(snapshot),
                                ..
                            },
                            RecurringReceiver::Existing { snapshot: original },
                        ) => *snapshot == original,
                        _ => false,
                    }
            })
            && self
                .privileged
                .as_ref()
                .is_none_or(|review| review.matches(&self.plan, &self.receiver))
    }

    pub(super) fn view(&self, recurring: bool) -> iced::widget::Column<'_, Message> {
        let vacant_server = self.first_server_tree_upload()
            || matches!(
                self.ordinary_receiver,
                Some(RecurringReceiver::Vacant { .. })
            );
        let receiver = match &self.receiver {
            MixedReceiver::Server {
                destination,
                snapshot,
            } => format!(
                "{} server folder · {}",
                if snapshot.is_some() {
                    "Existing"
                } else if vacant_server {
                    "Vacant"
                } else {
                    "New"
                },
                destination.as_str()
            ),
            MixedReceiver::Local {
                destination,
                snapshot,
                vacant,
            } => format!(
                "{} local folder · {}",
                if snapshot.is_some() {
                    "Existing"
                } else if vacant.is_some() {
                    "Vacant"
                } else {
                    "New"
                },
                destination.display()
            ),
        };
        let publication = match &self.receiver {
            MixedReceiver::Server { .. } if vacant_server => Some(
                "The first successful firing creates this folder. Later firings replace only the last confirmed folder and retain it for recovery.",
            ),
            MixedReceiver::Local {
                vacant: Some(_), ..
            } if recurring => Some(
                "The first successful firing creates this folder. Later firings replace only the last confirmed folder and retain it for recovery.",
            ),
            MixedReceiver::Local {
                vacant: Some(_), ..
            } => Some("The run creates this folder only while the reviewed destination is vacant."),
            MixedReceiver::Local {
                snapshot: Some(_), ..
            } if recurring => Some(
                "The first successful firing replaces this exact folder and retains it for recovery. Later firings replace each confirmed successor.",
            ),
            _ => None,
        };
        let parent_creation = match &self.receiver {
            MixedReceiver::Local {
                destination,
                vacant: Some(vacant),
                ..
            } => super::parent_creation_detail(destination, vacant),
            _ => None,
        };
        let removal = match &self.plan.delete {
            DeleteScope::Retain => "Keep unmatched receiver entries.".to_owned(),
            DeleteScope::All => {
                "Remove unmatched entries throughout this destination folder.".to_owned()
            }
            DeleteScope::Named(names) if names.is_empty() => {
                "Prune only at exact file replacements.".to_owned()
            }
            DeleteScope::Named(names) => {
                format!("Remove unmatched entries in {}.", names.join(", "))
            }
        };
        let inventory = self.plan.inventory;
        let mut details = column![
            self.privileged
                .as_ref()
                .map(|_| text("Passwordless sudo · configured Linux helper")),
            text(receiver),
            parent_creation.map(|detail| text(detail).wrapping(text::Wrapping::Glyph).size(12)),
            publication.map(|message| text(message).size(12)),
            text(format!(
                "{} files · {} folders · {} bytes in the projected transfer",
                inventory.files,
                inventory.entries - inventory.files,
                inventory.bytes
            ))
            .size(12),
            text(removal).size(12),
        ]
        .spacing(4);
        for (index, source) in self.plan.sources.iter().enumerate() {
            let placement = match &source.placement {
                Placement::File(_) => "File",
                Placement::Named(_) => "Folder by name",
                Placement::Contents => "Folder contents",
            };
            details = details.push(
                text(format!("{} · {placement} · {}", index + 1, source.raw))
                    .wrapping(text::Wrapping::Glyph)
                    .size(12),
            );
        }
        for collision in &self.plan.collisions {
            let description = match collision.kind {
                CollisionKind::DirectoryCoalesced => format!(
                    "combines folders from sources {} and {}",
                    collision.winner + 1,
                    collision.loser + 1
                ),
                CollisionKind::DirectoryReplacedFile | CollisionKind::EarlierFile => format!(
                    "uses source {} in place of source {}",
                    collision.winner + 1,
                    collision.loser + 1
                ),
            };
            let relative = if collision
                .relative
                .chars()
                .all(crate::session_journal::is_display_character)
            {
                collision.relative.clone()
            } else {
                format!("{:?}", collision.relative)
            };
            details = details.push(
                text(format!("{relative} · {description}"))
                    .wrapping(text::Wrapping::Glyph)
                    .size(12),
            );
        }
        let more = self.plan.collision_count as usize - self.plan.collisions.len();
        details
            .push((more != 0).then(|| text(format!("{more} further overlapping paths")).size(12)))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedPlan {
    pub(crate) sources: Vec<MixedSourceReview>,
    pub(crate) inventory: Inventory,
    pub(crate) delete: DeleteScope,
    pub(crate) collision_count: u32,
    pub(crate) collisions: Vec<MixedCollision>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedSourceReview {
    pub(crate) raw: String,
    pub(crate) placement: Placement,
    pub(crate) inventory: Inventory,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MixedCollision {
    pub(crate) relative: String,
    pub(crate) winner: usize,
    pub(crate) loser: usize,
    pub(crate) kind: CollisionKind,
}

impl MixedPlan {
    pub(crate) fn from_projection(
        target: &MixedReviewTarget,
        inputs: &[Source],
        projected: &Projection,
    ) -> Result<Self, String> {
        if target.sources.len() != inputs.len() {
            return Err(BAD_REVIEW.into());
        }
        let sources = target
            .sources
            .iter()
            .zip(inputs)
            .map(|(raw, source)| {
                let inventory = projection::project(std::slice::from_ref(source), false)
                    .map_err(|error| error.to_string())?
                    .inventory;
                Ok::<_, String>(MixedSourceReview {
                    raw: raw.clone(),
                    placement: source.placement.clone(),
                    inventory,
                })
            })
            .collect::<Result<_, _>>()?;
        let plan = Self {
            sources,
            inventory: projected.inventory,
            delete: projected.delete.clone(),
            collision_count: u32::try_from(projected.collisions.len()).map_err(|_| BAD_REVIEW)?,
            collisions: projected
                .collisions
                .iter()
                .take(COLLISION_SAMPLES)
                .map(|collision| MixedCollision {
                    relative: collision.relative.clone(),
                    winner: collision.winner,
                    loser: collision.loser,
                    kind: collision.kind,
                })
                .collect(),
        };
        if !plan.matches(&target.sources, &target.destination, target.delete) {
            return Err(BAD_REVIEW.into());
        }
        Ok(plan)
    }

    pub(super) fn valid(&self) -> bool {
        if self.sources.is_empty() || self.sources.len() > MAX_SOURCES {
            return false;
        }
        // Each source inventory already counts its named root or projected file.
        let combined = self.sources.iter().try_fold(
            (0_u32, 0_u32, 0_u64),
            |(entries, files, bytes), source| {
                let inventory = source.inventory;
                if !inventory.valid() {
                    return None;
                }
                Some((
                    entries.checked_add(inventory.entries)?,
                    files.checked_add(inventory.files)?,
                    bytes.checked_add(inventory.bytes)?,
                ))
            },
        );
        combined.is_some_and(|(entries, files, bytes)| {
            entries as usize <= MAX_TREE_ENTRIES && files <= entries && bytes <= MAX_FILE_BYTES
        }) && self.inventory.valid()
            && self.delete.valid()
            && self.collision_count as usize <= MAX_SOURCES * MAX_TREE_ENTRIES
            && self.collisions.len() == (self.collision_count as usize).min(COLLISION_SAMPLES)
            && self.collisions.iter().all(|collision| {
                collision.winner < self.sources.len()
                    && collision.loser < self.sources.len()
                    && collision.winner != collision.loser
                    && collision.relative.len() <= 4096
                    && collision.relative.split('/').all(valid_name)
            })
    }

    pub(super) fn matches(
        &self,
        raw: &[String],
        destination: &MixedDestination,
        mirror: bool,
    ) -> bool {
        if !self.valid() || self.sources.len() != raw.len() {
            return false;
        }
        let upload = matches!(destination, MixedDestination::Server(_));
        for (source, raw) in self.sources.iter().zip(raw) {
            let Ok(name) = literal_name(raw, upload) else {
                return false;
            };
            if source.raw != *raw {
                return false;
            }
            match &source.placement {
                Placement::File(leaf) | Placement::Named(leaf)
                    if !source_contents(raw, upload) && leaf == &name => {}
                Placement::Contents if source_contents(raw, upload) => {}
                _ => return false,
            }
        }
        projection::delete_scope(self.sources.iter().map(|source| &source.placement), mirror)
            .is_ok_and(|expected| self.delete == expected)
    }
}

impl Transfer {
    pub(crate) fn mixed_target(
        &self,
        reference: &ConnectionRef,
        upload: bool,
    ) -> Result<MixedReviewTarget, &'static str> {
        let sources = self.mixed.as_ref().ok_or(BAD_SOURCES)?;
        let (settings, destination) = if upload {
            (self.existing, MixedDestination::Server(self.remote.clone()))
        } else {
            (
                self.existing_download,
                MixedDestination::Local(self.local.clone()),
            )
        };
        let settings =
            settings.ok_or("Choose one destination root for the mixed rsync transfer.")?;
        Ok(MixedReviewTarget {
            reference: reference.clone(),
            sources: sources.clone(),
            destination,
            delete: settings.delete,
            elevation: self.elevation,
            review_vacant: if upload {
                self.allow_vacant_server_tree_upload
            } else {
                self.standing.is_some() || self.allow_vacant_directory_download
            },
        })
    }

    pub(super) fn validate_mixed(
        &self,
        sources: &[String],
        upload: bool,
    ) -> Result<(), &'static str> {
        let settings = if upload {
            self.existing.filter(|_| self.existing_download.is_none())
        } else {
            self.existing_download.filter(|_| self.existing.is_none())
        };
        if !self.directory
            || !self.additional_sources.is_empty()
            || settings.is_none_or(|settings| settings.include_source_name)
            || !(1..=MAX_SOURCES).contains(&sources.len())
        {
            return Err(BAD_SOURCES);
        }
        for raw in sources {
            literal_name(raw, upload)?;
            if upload
                && local_scope_path(Path::new(bare_source(raw, true)))?
                    == local_scope_path(&self.program)?
            {
                return Err("Choose rsync sources separate from the approved executable.");
            }
        }
        if self.uses_passwordless_sudo() {
            let valid = if upload {
                kortty_server_archive::privileged_rsync::valid_path(self.remote.as_str())
            } else {
                sources.iter().all(|source| {
                    kortty_server_archive::privileged_rsync::valid_path(bare_source(source, false))
                })
            };
            if !valid {
                return Err("Choose literal absolute paths under the configured Linux sudo roots.");
            }
        }
        let first_matches = if upload {
            self.local == Path::new(bare_source(&sources[0], true))
        } else {
            self.remote.as_str() == bare_source(&sources[0], false)
        };
        if !first_matches {
            return Err(BAD_SOURCES);
        }
        Ok(())
    }
}

pub(crate) fn bare_source(raw: &str, upload: bool) -> &str {
    if cfg!(windows) && upload {
        raw.trim_end_matches(['/', '\\'])
    } else {
        raw.trim_end_matches('/')
    }
}

pub(crate) fn source_contents(raw: &str, upload: bool) -> bool {
    raw.len() != bare_source(raw, upload).len()
}

pub(in crate::scheduler_window) fn source_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_matches(|character| character <= ' '))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn literal_name(raw: &str, upload: bool) -> Result<String, &'static str> {
    let source = bare_source(raw, upload);
    if raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err(BAD_SOURCES);
    }
    let name = if upload {
        let path = Path::new(source);
        super::super::remote::validate_local_path(path)?;
        #[cfg(windows)]
        let source = source.strip_prefix(r"\\?\").unwrap_or(source);
        if source.contains(['*', '?', '[', ']']) {
            return Err(BAD_SOURCES);
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or(BAD_SOURCES)?
            .to_owned()
    } else {
        if !valid_remote(source) {
            return Err(BAD_SOURCES);
        }
        RemotePath::try_from(source.to_owned())?
            .basename()
            .to_owned()
    };
    valid_name(&name).then_some(name).ok_or(BAD_SOURCES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_server_archive::rsync_tree::projection::Entry;
    use serde_json::json;

    #[test]
    fn mixed_rsync_plan_binds_order_layout_delete_and_receiver() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let first = json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
        let reference = serde_json::from_value(first.clone()).unwrap();
        let program = root.join("rsync");
        let local = root.join("receiver");
        let target = MixedReviewTarget {
            reference,
            sources: vec!["/site/info".into(), "/site/notes/".into()],
            destination: MixedDestination::Local(local.clone()),
            delete: true,
            elevation: Elevation::Ordinary,
            review_vacant: false,
        };
        let inputs = [
            Source {
                placement: Placement::File("info".into()),
                entries: vec![Entry {
                    relative: "".into(),
                    file: Some((5, [1; 32])),
                }],
            },
            Source {
                placement: Placement::Contents,
                entries: vec![Entry {
                    relative: "note".into(),
                    file: Some((3, [2; 32])),
                }],
            },
        ];
        let projection = projection::project(&inputs, true).unwrap();
        let plan = MixedPlan::from_projection(&target, &inputs, &projection).unwrap();
        assert_eq!(plan.delete, DeleteScope::All);
        assert_eq!(plan.inventory.files, 2);
        let review = MixedReview {
            plan,
            receiver: MixedReceiver::Local {
                destination: local.clone(),
                snapshot: None,
                vacant: None,
            },
            privileged: None,
            ordinary_receiver: None,
        };
        let transfer = json!({
            "program": program, "local": local, "remote": "/site/info", "directory": true,
            "existing_download": {"delete": true, "include_source_name": false}, "mixed": target.sources,
            "approval": {"program": program, "digest": vec![8u8; 32], "reference": first,
                "pins": ["SHA256:fixture"], "mixed": review},
        });
        let decode = |value| serde_json::from_value::<Transfer>(value).unwrap();
        let restored = decode(transfer.clone());
        assert!(restored.validate(false).is_ok());
        assert!(restored.validate(true).is_err());
        let reviewed_target = restored.mixed_target(&target.reference, false).unwrap();
        assert!(
            reviewed_target.matches(
                restored
                    .approval
                    .as_ref()
                    .unwrap()
                    .mixed
                    .as_deref()
                    .unwrap()
            )
        );

        let mut reordered = transfer.clone();
        reordered["mixed"] = json!(["/site/notes/", "/site/info"]);
        reordered["remote"] = json!("/site/notes");
        assert!(decode(reordered).validate(false).is_err());
        let mut wrong_slash = transfer.clone();
        wrong_slash["mixed"][1] = json!("/site/notes");
        assert!(decode(wrong_slash).validate(false).is_err());
        let mut wrong_receiver = transfer.clone();
        wrong_receiver["local"] = json!(root.join("other"));
        assert!(decode(wrong_receiver).validate(false).is_err());
        let mut wrong_delete = transfer.clone();
        wrong_delete["existing_download"]["delete"] = json!(false);
        assert!(decode(wrong_delete).validate(false).is_err());
        let mut oversized_bytes = transfer.clone();
        for source in oversized_bytes["approval"]["mixed"]["plan"]["sources"]
            .as_array_mut()
            .unwrap()
        {
            source["inventory"]["bytes"] = json!(MAX_FILE_BYTES / 2 + 1);
        }
        assert!(decode(oversized_bytes).validate(false).is_err());
        let mut oversized_entries = transfer.clone();
        for source in oversized_entries["approval"]["mixed"]["plan"]["sources"]
            .as_array_mut()
            .unwrap()
        {
            source["inventory"]["entries"] = json!(MAX_TREE_ENTRIES / 2 + 1);
        }
        assert!(decode(oversized_entries).validate(false).is_err());
        let mut no_mixed = transfer;
        no_mixed.as_object_mut().unwrap().remove("mixed");
        assert!(decode(no_mixed).validate(false).is_err());
    }

    #[test]
    fn mixed_passwordless_upload_binds_the_receiver_and_projected_delete_scope() {
        let reference = serde_json::from_value(json!({
            "id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}
        }))
        .unwrap();
        let source = std::env::current_dir().unwrap().join("source");
        let raw = source.to_str().unwrap().to_owned();
        let path = "/protected/receiver";
        let destination = RemotePath::try_from(path.to_owned()).unwrap();
        let target = MixedReviewTarget {
            reference,
            sources: vec![raw],
            destination: MixedDestination::Server(destination.clone()),
            delete: true,
            elevation: Elevation::Passwordless,
            review_vacant: false,
        };
        let inputs = [Source {
            placement: Placement::Named("source".into()),
            entries: Vec::new(),
        }];
        let projection = projection::project(&inputs, true).unwrap();
        let original = ExistingTree {
            inventory: kortty_server_archive::rsync_tree::Manifest::default().finish(),
            fingerprint: [7; 32],
        };
        let helper = PrivilegedReview {
            spec: kortty_server_archive::privileged_rsync::Spec {
                path: path.to_owned(),
                operation: Operation::Upload {
                    directory: true,
                    replace: true,
                    delete: projection.delete.clone(),
                },
            },
            policy: [6; 32],
            fingerprint: original.fingerprint,
            inventory: Some(original.inventory),
            handoff: "/run/kortty/rsync-handoff".into(),
            first_file: false,
            first_tree: false,
        };
        let review = MixedReview {
            plan: MixedPlan::from_projection(&target, &inputs, &projection).unwrap(),
            receiver: MixedReceiver::Server {
                destination,
                snapshot: Some(original),
            },
            privileged: Some(MixedPrivilegedReview::Upload(Box::new(helper))),
            ordinary_receiver: None,
        };
        assert!(target.matches(&review));
        let mut ordinary = target.clone();
        ordinary.elevation = Elevation::Ordinary;
        assert!(!ordinary.matches(&review));
        let mut ordinary_review = review.clone();
        ordinary_review.privileged = None;
        assert!(ordinary.matches(&ordinary_review));
        ordinary.review_vacant = true;
        assert!(!ordinary.matches(&ordinary_review));
        ordinary_review.ordinary_receiver =
            Some(RecurringReceiver::Existing { snapshot: original });
        assert!(ordinary.matches(&ordinary_review));
        ordinary_review.ordinary_receiver = Some(RecurringReceiver::Vacant {
            fingerprint: [4; 32],
        });
        assert!(!ordinary.matches(&ordinary_review));
        let MixedReceiver::Server { snapshot, .. } = &mut ordinary_review.receiver else {
            panic!("expected server folder");
        };
        *snapshot = None;
        assert!(ordinary.matches(&ordinary_review));

        let mut vacant = review.clone();
        let MixedReceiver::Server { snapshot, .. } = &mut vacant.receiver else {
            panic!("expected server folder");
        };
        *snapshot = None;
        let Some(MixedPrivilegedReview::Upload(proof)) = &mut vacant.privileged else {
            panic!("expected protected upload");
        };
        proof.first_tree = true;
        proof.inventory = None;
        assert!(!target.matches(&vacant));
        let mut recurring = target.clone();
        recurring.review_vacant = true;
        assert!(recurring.matches(&vacant));
        assert!(recurring.matches(&review));
        let Some(MixedPrivilegedReview::Upload(proof)) = &mut vacant.privileged else {
            panic!("expected protected upload");
        };
        proof.spec.operation = Operation::Upload {
            directory: true,
            replace: true,
            delete: DeleteScope::All,
        };
        assert!(!recurring.matches(&vacant));
        let Some(MixedPrivilegedReview::Upload(proof)) = &mut vacant.privileged else {
            panic!("expected protected upload");
        };
        proof.first_tree = false;
        proof.spec.operation = Operation::Upload {
            directory: true,
            replace: false,
            delete: DeleteScope::Retain,
        };
        assert!(target.matches(&vacant));
        assert!(!recurring.matches(&vacant));

        let mut tampered = review.clone();
        let Some(MixedPrivilegedReview::Upload(proof)) = &mut tampered.privileged else {
            panic!("expected protected upload");
        };
        proof.spec.operation = Operation::Upload {
            directory: true,
            replace: true,
            delete: DeleteScope::All,
        };
        assert!(!target.matches(&tampered));

        let mut tampered = review;
        let Some(MixedPrivilegedReview::Upload(proof)) = &mut tampered.privileged else {
            panic!("expected protected upload");
        };
        proof.fingerprint = [8; 32];
        assert!(!target.matches(&tampered));
    }
}
