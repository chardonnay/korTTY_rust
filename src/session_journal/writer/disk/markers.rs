//! Marker assignments and portable definitions kept by the journal writer.

use std::collections::HashSet;
use std::io;

use crate::session_journal::entry_edit::{LegacyMarker, MarkerChoice, MarkerSource};
use crate::session_journal::markers::{Compiled, Definition, match_text};

use super::{Entry, MAX_TIMELINE, element, invalid};

#[derive(Eq, PartialEq)]
pub(super) struct Assignment {
    pub(super) id: Option<String>,
    pub(super) legacy: LegacyMarker,
    pub(super) source: MarkerSource,
}

impl Assignment {
    pub(super) fn new(definition: &Definition, source: MarkerSource) -> Self {
        Self {
            id: (!definition.is_builtin()).then(|| definition.id.clone()),
            legacy: definition.legacy,
            source,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Snapshots {
    definitions: Vec<Definition>,
}

impl Snapshots {
    pub(super) fn manual_choice(
        &self,
        marker: &MarkerChoice,
        current: &Compiled,
    ) -> io::Result<Definition> {
        let definition = marker.definition();
        if !marker.is_builtin() {
            let expected = if marker.is_profile_definition() {
                current.find(marker.id())
            } else {
                self.definitions
                    .iter()
                    .find(|definition| definition.id == marker.id())
            };
            if expected != Some(&definition) {
                return Err(invalid(
                    "The selected live journal marker changed or is unavailable",
                ));
            }
        }
        Ok(definition)
    }

    pub(super) fn upsert(&mut self, definition: &Definition) -> io::Result<()> {
        if definition.is_builtin() {
            return Ok(());
        }
        if let Some(existing) = self
            .definitions
            .iter_mut()
            .find(|old| old.id == definition.id)
        {
            existing.clone_from(definition);
        } else {
            if self.definitions.len() == MAX_TIMELINE {
                return Err(invalid(
                    "Session journal marker snapshots exceed their storage limit",
                ));
            }
            self.definitions.push(definition.clone());
        }
        Ok(())
    }

    pub(super) fn prune_unused(&mut self, entries: &[Entry]) {
        if self.definitions.is_empty() {
            return;
        }
        let used: HashSet<_> = entries
            .iter()
            .filter_map(|entry| entry.marker.as_ref()?.id.as_deref())
            .collect();
        self.definitions
            .retain(|definition| used.contains(definition.id.as_str()));
    }

    pub(super) fn write_xml(&self, xml: &mut String) {
        if self.definitions.is_empty() {
            return;
        }
        xml.push_str("<markerDefinitions>\n");
        for definition in &self.definitions {
            xml.push_str("<marker>\n");
            element(xml, "id", &definition.id);
            element(xml, "name", &definition.name);
            if let Some([red, green, blue]) = definition.color {
                element(xml, "color", &format!("#{red:02x}{green:02x}{blue:02x}"));
            }
            element(xml, "builtIn", "false");
            element(xml, "legacyMarker", definition.legacy.xml());
            xml.push_str("</marker>\n");
        }
        xml.push_str("</markerDefinitions>\n");
    }
}

pub(super) fn first_match<'a>(rules: &'a Compiled, entry: &Entry) -> Option<&'a Definition> {
    // The writer only creates native notes, screenshots and status entries. Imported input
    // and output excerpts are handled by the stopped-journal XML transaction.
    let text = match_text([
        entry.fields.title.as_str(),
        entry.fields.text.as_str(),
        entry.fields.note.as_str(),
    ]);
    rules.first_match(&text)
}
