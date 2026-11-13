//! Private JAXB snippet-library migration from Java korTTY 2.26.0.

use std::collections::{HashMap, HashSet};
use std::io;

use roxmltree::Node;

use super::{
    Imports, Portable, boolean, check_count, invalid, read_xml_field, xml_elements, xml_is,
    xml_text,
};
use crate::snippets::{MAX_CONTENT_BYTES, SnippetId, diagrams, folders};

pub(super) fn read(root: Node<'_, '_>, imports: &mut Imports) -> io::Result<()> {
    let mut snippets = Vec::new();
    let mut folders = None;
    let mut systems = false;
    for child in xml_elements(root)? {
        if xml_is(child, "snippet") {
            check_count(imports.structured.len() + imports.text.len() + snippets.len() + 1)?;
            snippets.push(child);
        } else if xml_is(child, "folders") && folders.is_none() {
            folders = Some(read_folders(child)?);
        } else if xml_is(child, "category") {
            read_category(child)?;
        } else if xml_is(child, "operatingSystems") && !systems {
            for system in xml_elements(child)? {
                if !xml_is(system, "os") {
                    return Err(invalid("Invalid Java snippet library operating system"));
                }
                xml_text(system)?;
            }
            systems = true;
        } else {
            return Err(invalid("Unknown or duplicate Java snippet library field"));
        }
    }
    let folders = folders.unwrap_or_default();
    if imports.folders.len().saturating_add(folders.len()) > folders::MAX_FOLDERS {
        return Err(invalid("Too many imported Java snippet folders"));
    }
    let mut ids = HashSet::new();
    for snippet in snippets {
        read_snippet(snippet, &folders, imports, &mut ids)?;
    }
    imports.folders.extend(folders.into_values());
    Ok(())
}

fn read_category(category: Node<'_, '_>) -> io::Result<()> {
    for (name, field) in fields(category)? {
        if !matches!(name, "id" | "name" | "sortOrder") {
            return Err(invalid("Unknown Java snippet category field"));
        }
        xml_text(field)?;
    }
    Ok(())
}

fn read_snippet(
    node: Node<'_, '_>,
    folders: &HashMap<folders::Id, folders::Folder>,
    imports: &mut Imports,
    ids: &mut HashSet<SnippetId>,
) -> io::Result<()> {
    let mut entry = Portable::default();
    let mut id = None;
    let mut diagrams = None;
    for (name, field) in fields(node)? {
        match name {
            "id" => id = Some(SnippetId::from_java_uuid(&xml_text(field)?)?),
            "folderId" => {
                let value = xml_text(field)?;
                if !value.is_empty() {
                    let id = folders::Id::from_java_uuid(&value)?;
                    entry.folder_path = Some(
                        folders
                            .get(&id)
                            .ok_or_else(|| invalid("A Java snippet refers to an unknown folder"))?
                            .path
                            .clone(),
                    );
                }
            }
            "diagrams" => diagrams = Some(field),
            "executable" => entry.executable = Some(java_boolean(&xml_text(field)?)?),
            "favorite"
            | "usageCount"
            | "lastUsed"
            | "createdAt"
            | "modifiedAt"
            | "historyMaxSize"
            | "codeTextLanguageCode" => {
                xml_text(field)?;
            }
            "history" => {
                for version in xml_elements(field)? {
                    if !xml_is(version, "entry") {
                        return Err(invalid("Invalid Java snippet history entry"));
                    }
                }
            }
            "folderPath" | "savedDiagrams" => {
                return Err(invalid(
                    "Unexpected portable XML field in a Java snippet library",
                ));
            }
            _ => {
                if !read_xml_field(&mut entry, field)? {
                    return Err(invalid("Unknown Java snippet field"));
                }
            }
        }
    }
    let id = id
        .filter(|id| ids.insert(*id))
        .ok_or_else(|| invalid("Missing or duplicate Java snippet ID"))?;
    if let Some(diagrams) = diagrams {
        let source = entry
            .content
            .as_deref()
            .ok_or_else(|| invalid("A snippet is missing its content"))?;
        if source.len() > MAX_CONTENT_BYTES {
            return Err(invalid("Java snippet content exceeds 10 MiB"));
        }
        entry.diagrams = read_diagrams(diagrams, source, imports)?;
    }
    imports.structured.push(entry.into_input(Some(id))?);
    Ok(())
}

fn java_boolean(value: &str) -> io::Result<bool> {
    match value.trim() {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => boolean(value),
    }
}

fn fields<'a, 'input>(node: Node<'a, 'input>) -> io::Result<Vec<(&'input str, Node<'a, 'input>)>> {
    let mut fields = Vec::new();
    let mut seen = HashSet::new();
    for field in xml_elements(node)? {
        let name = field.tag_name().name();
        if !xml_is(field, name) || !seen.insert(name) {
            return Err(invalid("Invalid or duplicate Java snippet library field"));
        }
        fields.push((name, field));
    }
    Ok(fields)
}

struct Folder {
    name: String,
    parent: Option<folders::Id>,
}

fn read_folders(node: Node<'_, '_>) -> io::Result<HashMap<folders::Id, folders::Folder>> {
    let mut raw = HashMap::new();
    for folder in xml_elements(node)? {
        if !xml_is(folder, "folder") || raw.len() == folders::MAX_FOLDERS {
            return Err(invalid("Invalid Java snippet folder collection"));
        }
        let mut id = None;
        let mut name = None;
        let mut parent = None;
        for (field_name, field) in fields(folder)? {
            let value = xml_text(field)?;
            match field_name {
                "id" => id = Some(folders::Id::from_java_uuid(&value)?),
                "name" => name = Some(folders::folder_name(&value)?),
                "parentId" if !value.is_empty() => {
                    parent = Some(folders::Id::from_java_uuid(&value)?)
                }
                "parentId" | "sortOrder" => {}
                _ => return Err(invalid("Unknown Java snippet folder field")),
            }
        }
        let id = id.ok_or_else(|| invalid("A Java snippet folder has no ID"))?;
        let name = name.ok_or_else(|| invalid("A Java snippet folder has no name"))?;
        if raw.insert(id, Folder { name, parent }).is_some() {
            return Err(invalid("Duplicate Java snippet folder ID"));
        }
    }
    let mut folders = HashMap::with_capacity(raw.len());
    let mut paths = HashSet::new();
    for id in raw.keys().copied() {
        let mut next = Some(id);
        let mut names = Vec::new();
        let mut seen = HashSet::new();
        while let Some(id) = next {
            if names.len() == folders::MAX_DEPTH || !seen.insert(id) {
                return Err(invalid("Cyclic or oversized Java snippet folder tree"));
            }
            let folder = raw
                .get(&id)
                .ok_or_else(|| invalid("Unknown Java snippet folder parent"))?;
            names.push(folder.name.as_str());
            next = folder.parent;
        }
        names.reverse();
        let path = names.join("/");
        if path.len() > folders::MAX_PATH_BYTES || !paths.insert(folders::path_key(&path)) {
            return Err(invalid("Duplicate or oversized Java snippet folder path"));
        }
        folders.insert(id, folders::Folder { id, path });
    }
    Ok(folders)
}

fn read_diagrams(
    node: Node<'_, '_>,
    source: &str,
    imports: &mut Imports,
) -> io::Result<Vec<diagrams::Saved>> {
    let mut saved = Vec::new();
    let mut ids = HashSet::new();
    for record in xml_elements(node)? {
        if !xml_is(record, "diagram") {
            return Err(invalid("Invalid Java snippet diagram collection"));
        }
        if saved.len() == diagrams::MAX_DIAGRAMS {
            imports.skipped_diagrams += 1;
            continue;
        }
        match read_diagram(record, source) {
            Ok((diagram, skipped)) if ids.insert(diagram.id) => {
                saved.push(diagram);
                imports.skipped_references += skipped;
            }
            _ => imports.skipped_diagrams += 1,
        }
    }
    Ok(saved)
}

fn read_diagram(node: Node<'_, '_>, source: &str) -> io::Result<(diagrams::Saved, usize)> {
    let mut id = None;
    let mut title = String::new();
    let mut family = diagrams::Family::default();
    let mut mermaid = String::new();
    let mut digest = None;
    let mut instructions = String::new();
    let (mut start, mut end) = (0, 0);
    let (mut created_at, mut updated_at) = (0, None);
    let mut references = Vec::new();
    let mut skipped = 0;
    for (name, field) in fields(node)? {
        if name == "codeReferences" {
            for reference in xml_elements(field)? {
                match read_reference(reference) {
                    Ok(reference) => references.push(reference),
                    Err(_) => skipped += 1,
                }
            }
            continue;
        }
        let value = xml_text(field)?;
        match name {
            "id" => id = Some(SnippetId::from_java_uuid(&value)?),
            "title" => title = value.trim().into(),
            "type" => {
                family = match value.trim() {
                    "" | "logical-structure" => diagrams::Family::LogicalStructure,
                    "sequence" => diagrams::Family::Sequence,
                    "state" => diagrams::Family::State,
                    "class" => diagrams::Family::Class,
                    "er" => diagrams::Family::Er,
                    _ => return Err(invalid("Unknown Java snippet diagram family")),
                }
            }
            "mermaidSource" => mermaid = value,
            "plantUmlSource" => {}
            "sourceContentSha256" => {
                digest = (!value.trim().is_empty()).then(|| value.trim().to_ascii_lowercase())
            }
            "customInstructions" => instructions = value,
            "scopeStartLine" => start = number(&value)?,
            "scopeEndLine" => end = number(&value)?,
            "createdAt" => created_at = number(&value)?,
            "updatedAt" => updated_at = Some(number(&value)?),
            _ => return Err(invalid("Unknown Java snippet diagram field")),
        }
    }
    if title.is_empty() {
        title = family.to_string();
    }
    let record = diagrams::JavaRecord {
        id: id.ok_or_else(|| invalid("A Java snippet diagram has no ID"))?,
        title,
        family,
        scope: if (start, end) == (0, 0) {
            diagrams::Scope::Whole
        } else {
            diagrams::Scope::Lines { start, end }
        },
        mermaid,
        full_sha256: digest,
        instructions,
        references,
        created_at,
        updated_at: updated_at.unwrap_or(created_at),
    };
    diagrams::Saved::from_java(record, source)
        .map(|(saved, invalid)| (saved, invalid + skipped))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_reference(node: Node<'_, '_>) -> io::Result<diagrams::Reference> {
    if !xml_is(node, "codeReference") {
        return Err(invalid("Invalid Java snippet code reference"));
    }
    let mut reference = diagrams::Reference {
        node: String::new(),
        start: 0,
        end: 0,
    };
    for (name, field) in fields(node)? {
        let value = xml_text(field)?;
        match name {
            "nodeId" => reference.node = value,
            "startLine" => reference.start = number(&value)?,
            "endLine" => reference.end = number(&value)?,
            "label" => {}
            _ => return Err(invalid("Unknown Java snippet code reference field")),
        }
    }
    Ok(reference)
}

fn number<T: std::str::FromStr>(value: &str) -> io::Result<T> {
    value
        .trim()
        .parse()
        .map_err(|_| invalid("Invalid Java snippet diagram number"))
}
