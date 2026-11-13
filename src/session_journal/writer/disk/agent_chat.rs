//! Agent cards and their chat copies are committed by the live capture FIFO.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use zeroize::Zeroizing;

use crate::session_journal::agent_chat::{Card, Snapshot};

use super::{
    Entry, MAX_TIMELINE, MAX_TIMELINE_TEXT_BYTES, MetadataError, Shared, Writer, element, invalid,
};

const MAX_AGENT_XML_BYTES: usize = 2 * 1024 * 1024;

pub(super) struct Agent {
    model: String,
    duration_ms: Option<u64>,
    tokens: Option<u64>,
    chat: Option<Linked>,
}

struct Linked {
    snapshot: Snapshot,
    linked_at: Timestamp,
    xml_bytes: usize,
}

impl Agent {
    pub(super) fn write_xml(&self, xml: &mut String) {
        if !self.model.is_empty() {
            element(xml, "agentModel", &self.model);
        }
        if let Some(duration) = self.duration_ms {
            element(xml, "agentDurationMillis", &duration.to_string());
        }
        if let Some(tokens) = self.tokens {
            element(xml, "agentTokens", &tokens.to_string());
        }
        if let Some(chat) = &self.chat {
            chat.snapshot.write_xml(xml, chat.linked_at);
        }
    }

    fn byte_len(&self) -> usize {
        self.model
            .len()
            .saturating_mul(6)
            .saturating_add(128)
            .saturating_add(self.chat.as_ref().map_or(0, |chat| chat.xml_bytes))
    }
}

pub(super) fn save_error(error: MetadataError) -> String {
    if error.published {
        format!(
            "The Agent journal update was saved, but its durability could not be confirmed ({}).",
            error.error
        )
    } else {
        format!("Could not save the Agent journal update ({}).", error.error)
    }
}

fn require_permit(permit: Option<&AtomicBool>) -> io::Result<()> {
    if permit.is_some_and(|permit| !permit.load(Ordering::Acquire)) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "The Agent chat handoff was revoked",
        ))
    } else {
        Ok(())
    }
}

impl Writer {
    pub(super) fn ensure_agent_card(
        &mut self,
        card: Card,
        shared: &Shared,
        permit: Option<&AtomicBool>,
    ) -> Result<(), MetadataError> {
        require_permit(permit)?;
        card.validate()?;
        self.directory.require_metadata()?;
        if let Some(entry) = self
            .document
            .entries
            .iter()
            .find(|entry| entry.id == card.id)
        {
            return if entry.kind == "AGENT" && entry.agent.is_some() {
                Ok(())
            } else {
                Err(invalid("The Agent journal reference belongs to another entry").into())
            };
        }
        let text_bytes = self
            .timeline_text_bytes
            .saturating_add(card.title.len())
            .saturating_add(card.answer.len());
        let agent = Agent {
            model: card.model,
            duration_ms: card.duration_ms,
            tokens: card.tokens,
            chat: None,
        };
        if self.document.entries.len() >= MAX_TIMELINE
            || text_bytes > MAX_TIMELINE_TEXT_BYTES
            || self.agent_bytes(None).saturating_add(agent.byte_len()) > MAX_AGENT_XML_BYTES
        {
            return Err(invalid("The live journal reached its Agent card limit").into());
        }
        self.log.sync()?;
        let mut entry = Entry::new("AGENT", Timestamp::now())?;
        entry.id = card.id;
        entry.fields.title = Zeroizing::new(card.title);
        entry.fields.text = Zeroizing::new(card.answer);
        entry.agent = Some(agent);
        let old_markers = self.document.markers.clone();
        self.apply_new_marker(&mut entry, shared);
        self.document.entries.push(entry);
        let result = self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            || require_permit(permit),
        );
        if result.as_ref().is_err_and(|error| !error.published) {
            self.document.entries.pop();
            self.document.markers = old_markers;
        } else {
            self.timeline_text_bytes = text_bytes;
            self.advance_timeline();
        }
        result
    }

    pub(super) fn link_agent_chat(
        &mut self,
        id: &str,
        snapshot: Snapshot,
        permit: &AtomicBool,
    ) -> Result<(), MetadataError> {
        let check = || require_permit(Some(permit));
        check()?;
        snapshot.validate()?;
        self.directory.require_metadata()?;
        let index = self.document.entry_index(id)?;
        let entry = &self.document.entries[index];
        let agent = entry
            .agent
            .as_ref()
            .filter(|_| entry.kind == "AGENT")
            .ok_or_else(|| invalid("The selected journal entry is not an Agent card"))?;
        let linked_at = agent
            .chat
            .as_ref()
            .map_or(snapshot.captured_at, |old| old.linked_at);
        let mut encoded = Zeroizing::new(String::new());
        snapshot.write_xml(&mut encoded, linked_at);
        if let Some(old) = &agent.chat {
            if old.snapshot.chat_id != snapshot.chat_id {
                return Err(invalid("The Agent card is linked to another chat").into());
            }
            let mut previous = Zeroizing::new(String::new());
            old.snapshot.write_xml(&mut previous, linked_at);
            if *previous == *encoded {
                return check().map_err(MetadataError::from);
            }
            if old.snapshot.frozen || snapshot.captured_at <= old.snapshot.captured_at {
                return Err(invalid("The Agent chat snapshot changed or is already frozen").into());
            }
        }
        let stored = self
            .agent_bytes(Some(index))
            .saturating_add(agent.model.len().saturating_mul(6))
            .saturating_add(128)
            .saturating_add(encoded.len());
        if stored > MAX_AGENT_XML_BYTES {
            return Err(invalid("The live journal reached its Agent chat storage limit").into());
        }
        self.log.sync()?;
        let previous = self.document.entries[index]
            .agent
            .as_mut()
            .unwrap()
            .chat
            .replace(Linked {
                snapshot,
                linked_at,
                xml_bytes: encoded.len(),
            });
        let result = self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            check,
        );
        if result.as_ref().is_err_and(|error| !error.published) {
            self.document.entries[index].agent.as_mut().unwrap().chat = previous;
        } else {
            self.advance_timeline();
        }
        result
    }

    fn agent_bytes(&self, excluded: Option<usize>) -> usize {
        self.document
            .entries
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != excluded)
            .filter_map(|(_, entry)| entry.agent.as_ref())
            .map(Agent::byte_len)
            .sum()
    }

    pub(super) fn freeze_agent_chats(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.document.entries {
            if let Some(chat) = entry.agent.as_mut().and_then(|agent| agent.chat.as_mut())
                && !chat.snapshot.frozen
            {
                chat.snapshot.frozen = true;
                changed = true;
            }
        }
        changed
    }
}
