//! Backup-only references to Java workflow header snippets.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use roxmltree::Node;

use crate::snippet_analysis::model::hardening::HeaderRef;
use crate::snippets::{self, SnippetId};
use crate::terminal_agent::swarm::workflow::Language;
use crate::workspace::snippet_access::workflow_headers::{Defaults, applies_to};

use super::{
    DISPLAY_SECTION, JavaDisplayPreferences, RawFields, WindowSettings, is_object, preference_node,
    preference_text,
};

pub(crate) type References = BTreeMap<Language, SnippetId>;

pub(crate) fn from_backup(bytes: &[u8]) -> io::Result<References> {
    let fields: RawFields = serde_json::from_slice(bytes)?;
    let Some(display) = fields.get(DISPLAY_SECTION) else {
        return Ok(References::new());
    };
    let display: JavaDisplayPreferences = serde_json::from_str(display.get())?;
    Ok(display.java_workflow_header_defaults)
}

pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> References {
    let list = match preference_node(root, "workflowHeaderDefaults") {
        Ok(Some(list)) => list,
        Ok(None) => return References::new(),
        Err(()) => {
            *omitted += 1;
            return References::new();
        }
    };
    let mut references = References::new();
    let mut seen = BTreeSet::new();
    for entry in list.children().filter(|node| node.is_element()) {
        match parse(entry) {
            Some((language, id)) if seen.insert(language) => {
                references.insert(language, id);
            }
            Some((language, _)) => {
                references.remove(&language);
                *omitted += 1;
            }
            None => *omitted += 1,
        }
    }
    references
}

fn parse(entry: Node<'_, '_>) -> Option<(Language, SnippetId)> {
    if !entry.has_tag_name("headerDefault")
        || entry.tag_name().namespace().is_some()
        || entry.attributes().len() != 0
    {
        return None;
    }
    let language = match preference_text(entry, "language").ok()?? {
        "BASH" => Language::Bash,
        "PYTHON" => Language::Python,
        "PERL" => Language::Perl,
        "RUBY" => Language::Ruby,
        "POWERSHELL" => Language::PowerShell,
        "WINDOWS_CMD" => Language::WindowsCmd,
        "APPLESCRIPT" => Language::AppleScript,
        "ANSIBLE" => Language::Ansible,
        _ => return None,
    };
    let id = SnippetId::from_java_uuid(preference_text(entry, "headerSnippetId").ok()??).ok()?;
    Some((language, id))
}

pub(crate) fn merge(
    references: &References,
    settings: Option<&[u8]>,
    archived_snippets: &[u8],
    installed_snippets: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let bindings = snippets::java_workflow_header_bindings(archived_snippets, installed_snippets)?;
    let mut fields: RawFields = match settings {
        Some(bytes) if is_object(bytes) => serde_json::from_slice(bytes)?,
        Some(bytes) => {
            serde_json::from_slice(&serde_json::to_vec(&WindowSettings::decode(bytes)?)?)?
        }
        None => RawFields::new(),
    };
    let mut defaults: Defaults = fields
        .get("workflow_header_defaults")
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    let mut changed = false;
    for (&language, id) in references {
        if defaults.get(language).is_none()
            && let Some((installed, metadata)) = bindings.get(id)
            && applies_to(metadata, language)
        {
            defaults.set(language, Some(HeaderRef::User(*installed)));
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    fields.insert(
        "workflow_header_defaults".into(),
        serde_json::value::to_raw_value(&defaults)?,
    );
    let result = serde_json::to_vec(&fields)?;
    WindowSettings::validate_backup(&result)?;
    Ok(Some(result))
}
