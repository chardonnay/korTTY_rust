//! Per-editor recovery ownership and the native recovery banners.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Border, Element, Length, Theme};

use super::draft_store::{Snapshot, Summary, hash_content};
use super::{Draft, Message as WindowMessage, SnippetWindow};
use crate::snippets::SnippetId;

const QUIET_TIME: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub enum Message {
    Restore(SnippetId),
    Discard(SnippetId),
    RestoreOrphans,
    DiscardOrphans,
}

pub(crate) struct Remove {
    pub key: SnippetId,
    pub revision: Option<String>,
}

#[derive(Default)]
pub(super) struct State {
    pub key: Option<SnippetId>,
    pub owned: bool,
    pub offered: Option<Summary>,
    checked: bool,
    changed_at: Option<Instant>,
    base_changed: bool,
}

impl State {
    pub(super) fn new(saved: Option<SnippetId>) -> Self {
        Self {
            key: saved,
            checked: saved.is_none(),
            ..Self::default()
        }
    }

    pub(super) fn changed(&mut self) {
        self.changed_at = Some(Instant::now());
    }

    pub(super) fn saved(&mut self) {
        self.owned = false;
        self.changed_at = None;
    }

    pub(super) fn update_base(&mut self, content: Option<&str>) {
        if let Some(offered) = &self.offered {
            self.base_changed = offered.base_hash.is_some()
                && offered.base_hash != content.and_then(|content| hash_content(content).ok());
        }
    }

    pub(super) fn removed(&self) -> Option<Remove> {
        self.key.filter(|_| self.owned).map(|key| Remove {
            key,
            revision: None,
        })
    }
}

impl Draft {
    pub(super) fn recovery_key(&mut self) -> io::Result<SnippetId> {
        if let Some(key) = self.recovery.key {
            return Ok(key);
        }
        let key = SnippetId::new()?;
        self.recovery.key = Some(key);
        Ok(key)
    }

    fn snapshot(&mut self) -> io::Result<Snapshot> {
        Ok(Snapshot {
            key: self.recovery_key()?,
            saved_id: self.id,
            saved_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64,
            name: self.name.clone(),
            language: self.language.clone(),
            category: self.category.clone(),
            metadata: self.metadata.clone(),
            content: self.content.text().into(),
            base_hash: self
                .base
                .as_ref()
                .map(|entry| hash_content(entry.content()))
                .transpose()?,
        })
    }

    fn matches_saved(&self, snapshot: &Snapshot) -> bool {
        self.base.as_ref().is_some_and(|saved| {
            self.name.trim() == saved.name().trim()
                && self.language.trim() == saved.language().trim()
                && self.category.trim() == saved.category().trim()
                && self.metadata == *saved.metadata()
                && &*snapshot.content == saved.content()
        })
    }

    fn apply_snapshot(&mut self, snapshot: &Snapshot) {
        self.name = snapshot.name.clone();
        self.language = snapshot.language.clone();
        self.category = snapshot.category.clone();
        self.metadata = snapshot.metadata.clone();
        self.content = iced::widget::text_editor::Content::with_text(&snapshot.content);
        self.structure.changed(None);
        self.content_changed = true;
        self.metadata_changed = true;
        self.dirty = true;
        self.recovery.key = Some(snapshot.key);
        self.recovery.owned = true;
        self.recovery.offered = None;
        self.recovery.checked = true;
        self.recovery.changed_at = None;
    }
}

impl SnippetWindow {
    pub(crate) fn live_recovery_keys(&self) -> Vec<SnippetId> {
        self.tabs
            .editors(&self.draft)
            .filter_map(|draft| draft.recovery.key)
            .collect()
    }

    pub(crate) fn sync_recovery(&mut self, catalog: &[Summary], unavailable: &[SnippetId]) {
        let mut removals = Vec::new();
        self.tabs.editors_mut(&mut self.draft).for_each(|draft| {
            if draft.recovery.offered.as_ref().is_some_and(|offered| {
                !catalog
                    .iter()
                    .any(|entry| entry.key == offered.key && entry.revision == offered.revision)
            }) {
                if catalog
                    .iter()
                    .any(|entry| Some(entry.key) == draft.recovery.key)
                {
                    draft.recovery.checked = false;
                }
                draft.recovery.offered = None;
            }
            if draft.recovery.checked {
                return;
            }
            draft.recovery.checked = true;
            let Some(key) = draft.recovery.key else {
                return;
            };
            let Some(summary) = catalog.iter().find(|entry| entry.key == key) else {
                return;
            };
            let Ok(current) = draft.snapshot() else {
                return;
            };
            if !draft.dirty && current.form_hash().ok().as_ref() == Some(&summary.form_hash) {
                removals.push(Remove {
                    key,
                    revision: Some(summary.revision.clone()),
                });
                return;
            }
            draft.recovery.base_changed =
                summary.base_hash.is_some() && summary.base_hash != current.base_hash;
            draft.recovery.offered = Some(summary.clone());
        });
        self.recovery_removals.extend(removals);
        self.orphan_drafts = catalog
            .iter()
            .filter(|entry| {
                !unavailable.contains(&entry.key)
                    && self
                        .library
                        .get(entry.saved_id.unwrap_or(entry.key))
                        .is_none()
            })
            .cloned()
            .collect();
    }

    pub(crate) fn recovery_choices(&self, message: &Message) -> Vec<Summary> {
        match message {
            Message::Restore(key) | Message::Discard(key) => self
                .draft
                .recovery
                .offered
                .iter()
                .filter(|entry| entry.key == *key)
                .cloned()
                .collect(),
            Message::RestoreOrphans | Message::DiscardOrphans => self.orphan_drafts.clone(),
        }
    }

    pub(crate) fn can_begin_recovery(&self) -> bool {
        self.hosted_close.is_none()
            && self.prompt.is_none()
            && self.variables.is_none()
            && self.copy.is_none()
            && self.archive.is_none()
            && self.details.is_none()
            && !self.folders.modal()
            && !self.busy()
    }

    pub(crate) fn set_recovery_busy(&mut self, busy: bool) {
        self.recovery_busy = busy;
    }

    pub(crate) fn recovery_is_orphan(&self, summary: &Summary) -> bool {
        self.library
            .get(summary.saved_id.unwrap_or(summary.key))
            .is_none()
    }

    pub(crate) fn discard_offered(&mut self, summary: &Summary) {
        if self
            .draft
            .recovery
            .offered
            .as_ref()
            .is_some_and(|old| old.key == summary.key)
        {
            self.draft.recovery.offered = None;
            self.draft.recovery.changed();
        }
        self.orphan_drafts.retain(|old| old.key != summary.key);
    }

    pub(crate) fn apply_recovered(&mut self, snapshot: &Snapshot, orphan: bool) -> io::Result<()> {
        if orphan {
            self.tabs.new_draft(&mut self.draft);
        } else if self.draft.recovery.key != Some(snapshot.key) {
            return Err(io::Error::other("The recovery target is no longer open"));
        }
        self.draft.apply_snapshot(snapshot);
        if !orphan {
            self.draft.recovery.changed();
        }
        self.tabs.pin();
        self.library_selection = self.draft.id;
        self.status = Some("Recovered unsaved snippet changes".into());
        Ok(())
    }

    pub(crate) fn recovery_failed(&mut self, message: String) {
        self.status = Some(format!("Snippet recovery failed ({message})"));
    }

    pub(crate) fn autosave_failed(&mut self, key: SnippetId, message: String) {
        if let Some(draft) = self
            .tabs
            .editors_mut(&mut self.draft)
            .find(|draft| draft.recovery.key == Some(key))
        {
            draft.recovery.changed();
        }
        self.recovery_failed(message);
    }

    pub(crate) fn draft_autosave_pending(&self) -> bool {
        self.tabs.editors(&self.draft).any(|draft| {
            draft.dirty
                && draft.recovery.checked
                && draft.recovery.offered.is_none()
                && draft.recovery.changed_at.is_some()
        })
    }

    pub(crate) fn due_drafts(&mut self, now: Instant) -> Vec<Arc<Snapshot>> {
        let mut snapshots = Vec::new();
        let mut failure = None;
        let mut removals = Vec::new();
        self.tabs.editors_mut(&mut self.draft).for_each(|draft| {
            if !draft.dirty
                || !draft.recovery.checked
                || draft.recovery.offered.is_some()
                || draft
                    .recovery
                    .changed_at
                    .is_none_or(|changed| now.saturating_duration_since(changed) < QUIET_TIME)
            {
                return;
            }
            match draft.snapshot() {
                Ok(snapshot) => {
                    draft.recovery.changed_at = None;
                    if draft.matches_saved(&snapshot) {
                        if let Some(remove) = draft.recovery.removed() {
                            removals.push(remove);
                        }
                        draft.recovery.owned = false;
                        draft.dirty = false;
                    } else {
                        draft.recovery.owned = true;
                        snapshots.push(Arc::new(snapshot));
                    }
                }
                Err(error) => {
                    draft.recovery.changed_at = None;
                    failure = Some(error.to_string());
                }
            }
        });
        self.recovery_removals.extend(removals);
        if let Some(error) = failure {
            self.recovery_failed(error);
        }
        snapshots
    }

    pub(crate) fn take_recovery_removals(&mut self) -> Vec<Remove> {
        std::mem::take(&mut self.recovery_removals)
    }

    pub(crate) fn discard_owned_recovery(&mut self) {
        self.recovery_removals.extend(
            self.tabs
                .editors(&self.draft)
                .filter_map(|draft| draft.recovery.removed()),
        );
        self.tabs
            .editors_mut(&mut self.draft)
            .for_each(|draft| draft.recovery.owned = false);
    }

    pub(super) fn close_recovery_tab(&mut self, id: super::tabs::Id) {
        if let Some(remove) = self
            .tabs
            .entry(&self.draft, id)
            .and_then(|draft| draft.recovery.removed())
        {
            self.recovery_removals.push(remove);
        }
        self.tabs.close(&mut self.draft, id);
    }

    pub(super) fn recovery_banner(&self) -> Element<'_, WindowMessage> {
        let mut banner = column![].spacing(6);
        if self.recovery_busy {
            return text("Processing snippet drafts…").into();
        }
        if let Some(summary) = &self.draft.recovery.offered {
            let when = draft_time(summary);
            let extra = if self.draft.recovery.base_changed {
                " The saved snippet changed since this draft."
            } else {
                ""
            };
            banner = banner.push(
                row![
                    text(format!("Unsaved changes from {when}.{extra}")).width(Length::Fill),
                    button("Restore")
                        .on_press(WindowMessage::Recovery(Message::Restore(summary.key))),
                    button("Discard draft")
                        .on_press(WindowMessage::Recovery(Message::Discard(summary.key))),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if let Some(latest) = self.orphan_drafts.first() {
            banner = banner.push(
                row![
                    text(format!(
                        "{} draft(s) without a saved snippet. Latest from {}.",
                        self.orphan_drafts.len(),
                        draft_time(latest),
                    ))
                    .width(Length::Fill),
                    button("Restore drafts")
                        .on_press(WindowMessage::Recovery(Message::RestoreOrphans)),
                    button("Discard drafts")
                        .on_press(WindowMessage::Recovery(Message::DiscardOrphans)),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        if self.draft.recovery.offered.is_none() && self.orphan_drafts.is_empty() {
            return banner.into();
        }
        container(scrollable(banner))
            .max_height(96)
            .width(Length::Fill)
            .padding([6, 8])
            .style(|theme: &Theme| {
                let warning = theme.palette().warning;
                container::Style {
                    background: Some(warning.scale_alpha(0.14).into()),
                    border: Border {
                        color: warning.scale_alpha(0.55),
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..container::Style::default()
                }
            })
            .into()
    }
}

fn draft_time(summary: &Summary) -> String {
    jiff::Timestamp::from_millisecond(summary.saved_at)
        .map(|time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%F %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| String::from("an earlier session"))
}
