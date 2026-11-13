//! Bounded snippet interchange and optional native diagram records.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use roxmltree::{Document, Node, ParsingOptions};
use serde::de::{self, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::exchange::{escape_xml, xml_elements, xml_is};
use super::folders;
use super::{
    Input, Library, MAX_LIBRARY_BYTES, MAX_SNIPPETS, MAX_TAGS, Metadata, Snippet, SnippetId,
    invalid, profile,
};

mod files;
mod java_library;
mod text_file;
mod yaml;

use super::diagrams;

pub(crate) use files::{
    AnalysisFile, CertificateInfo, OpenPgpSettings, ZipSettings, analysis_files,
    encrypt_profile_zip, encrypt_saved_payload, inspect_certificate,
};
pub(super) use files::{
    MAX_CERTIFICATE_BYTES, enroll_certificate, normalize_fingerprint, validate_backup_certificate,
};
#[cfg(test)]
pub(crate) use tests::test_openpgp_key;

const VERSION: u8 = 1;
const MAX_PATHS: usize = 256;

#[derive(Clone, Default)]
pub(crate) struct Imports {
    pub(super) structured: Vec<ImportedSnippet>,
    pub(super) folders: Vec<folders::Folder>,
    pub(super) text: Vec<Input>,
    pub(crate) rejected_text: usize,
    pub(crate) skipped_diagrams: usize,
    pub(crate) skipped_references: usize,
}

#[derive(Clone)]
pub(super) struct ImportedSnippet {
    pub(super) id: Option<SnippetId>,
    pub(super) input: Input,
    pub(super) metadata: Metadata,
}

impl Imports {
    pub(crate) fn java_folder_count(&self) -> usize {
        self.folders.len()
    }
}

/// A fixed library subtree and its layout relative to the selected folder's parent.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct FolderSelection {
    root: String,
    directories: Vec<String>,
    entries: Vec<FolderEntry>,
}

#[derive(Clone, Eq, PartialEq)]
struct FolderEntry {
    snippet: Snippet,
    directory: String,
}

impl FolderSelection {
    pub(crate) fn new(folder: Option<folders::Id>, library: &Library) -> io::Result<Self> {
        let root = match folder {
            Some(id) => library
                .folder(id)
                .map(|folder| folder.path.as_str())
                .ok_or_else(|| invalid("The snippet folder no longer exists"))?,
            None => "",
        };
        let parent_prefix = root.rfind('/').map_or("", |separator| &root[..=separator]);
        let directories: Vec<_> = library
            .folders()
            .iter()
            .map(|folder| folder.path.as_str())
            .filter(|path| {
                root.is_empty()
                    || *path == root
                    || path
                        .strip_prefix(root)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
            .map(|path| path[parent_prefix.len()..].to_owned())
            .collect();
        let selected: HashSet<_> = library
            .snippets_in_folder(folder, true)
            .into_iter()
            .collect();
        let entries = library
            .entries()
            .iter()
            .filter(|snippet| selected.contains(&snippet.id()))
            .map(|snippet| {
                let directory = library
                    .folder_id(snippet)
                    .and_then(|id| library.folder(id))
                    .map_or("", |folder| folder.path.as_str());
                let directory = directory
                    .strip_prefix(parent_prefix)
                    .ok_or_else(|| invalid("A snippet is outside the selected folder"))?;
                Ok(FolderEntry {
                    snippet: snippet.clone(),
                    directory: directory.into(),
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        if entries.is_empty() && directories.is_empty() {
            return Err(invalid(
                "There are no snippet folders or snippets to export",
            ));
        }
        Ok(Self {
            root: root.into(),
            directories,
            entries,
        })
    }

    pub(crate) fn label(&self) -> &str {
        if self.root.is_empty() {
            "All snippets"
        } else {
            &self.root
        }
    }

    pub(crate) fn snippets(&self) -> impl ExactSizeIterator<Item = &Snippet> {
        self.entries.iter().map(|entry| &entry.snippet)
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum FolderTarget {
    Directory,
    Zip(ZipSettings),
    OpenPgp(OpenPgpSettings),
}

#[derive(PartialEq, Eq)]
pub(crate) enum Action {
    Import(Vec<PathBuf>),
    Export(PathBuf, Vec<Snippet>),
    ExportDirectory(PathBuf, Vec<Snippet>),
    ExportZip(PathBuf, Vec<Snippet>, ZipSettings),
    ExportOpenPgp(PathBuf, Vec<Snippet>, OpenPgpSettings),
    ExportFolder(PathBuf, FolderSelection, FolderTarget),
}

#[derive(Clone)]
pub(crate) enum Outcome {
    Imported(Arc<Imports>),
    Exported(usize),
}

impl Action {
    pub(crate) fn run(self) -> io::Result<Outcome> {
        match self {
            Self::Import(paths) => {
                import_files(paths).map(|imports| Outcome::Imported(Arc::new(imports)))
            }
            Self::Export(path, entries) => {
                absolute(&path)?;
                let format = Format::of(&path)
                    .ok_or_else(|| invalid("Use a .json, .xml, .yaml or .yml export path"))?;
                let encoded = format.write(&entries)?;
                if encoded.len() as u64 > MAX_LIBRARY_BYTES {
                    return Err(invalid("The snippet export exceeds 64 MiB"));
                }
                profile::save_new_atomic(&path, encoded.as_bytes())?;
                Ok(Outcome::Exported(entries.len()))
            }
            Self::ExportDirectory(path, entries) => {
                files::directory(&path, files::Source::Snippets(&entries))?;
                Ok(Outcome::Exported(entries.len()))
            }
            Self::ExportZip(path, entries, settings) => {
                files::zip(&path, files::Source::Snippets(&entries), settings)?;
                Ok(Outcome::Exported(entries.len()))
            }
            Self::ExportOpenPgp(path, entries, settings) => {
                files::encrypted_zip(&path, files::Source::Snippets(&entries), settings)?;
                Ok(Outcome::Exported(entries.len()))
            }
            Self::ExportFolder(path, folder, target) => {
                let source = files::Source::Folder(&folder);
                match target {
                    FolderTarget::Directory => files::directory(&path, source)?,
                    FolderTarget::Zip(settings) => files::zip(&path, source, settings)?,
                    FolderTarget::OpenPgp(settings) => {
                        files::encrypted_zip(&path, source, settings)?
                    }
                }
                Ok(Outcome::Exported(folder.entries.len()))
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Format {
    Json,
    Xml,
    Yaml,
}

impl Format {
    fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "xml" => Some(Self::Xml),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    fn looks_like_archive(self, source: &str) -> bool {
        match self {
            Self::Json => source.contains("\"snippets\""),
            Self::Xml => ["<snippetExport", "<snippet>", "<snippets"]
                .iter()
                .any(|marker| source.contains(marker)),
            Self::Yaml => source
                .lines()
                .any(|line| line.trim().starts_with("snippets:")),
        }
    }

    fn import(self, source: &str, imports: &mut Imports) -> io::Result<()> {
        let entries = match self {
            Self::Json => {
                let archive: JsonArchive = serde_json::from_str(source)
                    .map_err(|_| invalid("Invalid snippet JSON export"))?;
                if archive.version.is_some_and(|version| version != VERSION) {
                    return Err(invalid("Unsupported snippet JSON export version"));
                }
                archive.snippets
            }
            Self::Xml => return read_xml(source, imports),
            Self::Yaml => yaml::read(source)?,
        };
        for entry in entries {
            imports.structured.push(entry.into_input(None)?);
        }
        Ok(())
    }

    fn write(self, entries: &[Snippet]) -> io::Result<String> {
        let entries: Vec<_> = entries.iter().map(Portable::from).collect();
        let date = jiff::Zoned::now().datetime().to_string();
        match self {
            Self::Json => {
                let mut output = serde_json::to_string_pretty(&JsonArchive {
                    version: Some(VERSION),
                    export_date: Some(date),
                    snippets: entries,
                })
                .map_err(|_| invalid("Could not encode snippet JSON"))?;
                output.push('\n');
                Ok(output)
            }
            Self::Xml => write_xml(&entries, &date),
            Self::Yaml => yaml::write(&entries, &date),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonArchive {
    version: Option<u8>,
    export_date: Option<String>,
    #[serde(deserialize_with = "deserialize_entries")]
    snippets: Vec<Portable>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Portable {
    name: Option<String>,
    content: Option<Arc<str>>,
    language: Option<String>,
    category: Option<String>,
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operating_system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executable: Option<bool>,
    #[serde(deserialize_with = "deserialize_tags")]
    tags: Vec<String>,
    #[serde(
        deserialize_with = "deserialize_diagrams",
        skip_serializing_if = "Vec::is_empty"
    )]
    diagrams: Vec<diagrams::Saved>,
}

fn deserialize_entries<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<Portable>, D::Error> {
    deserialize_list::<_, _, MAX_SNIPPETS>(decoder)
}

fn deserialize_tags<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<String>, D::Error> {
    deserialize_list::<_, _, MAX_TAGS>(decoder)
}

fn deserialize_diagrams<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<diagrams::Saved>, D::Error> {
    deserialize_list::<_, _, { diagrams::MAX_DIAGRAMS }>(decoder)
}

fn read_diagrams(value: &str) -> io::Result<Vec<diagrams::Saved>> {
    let mut decoder = serde_json::Deserializer::from_str(value);
    let records = deserialize_diagrams(&mut decoder)
        .map_err(|_| invalid("Could not decode saved snippet diagrams"))?;
    decoder
        .end()
        .map_err(|_| invalid("Invalid saved snippet diagram data"))?;
    Ok(records)
}

fn write_diagrams(entries: &[diagrams::Saved]) -> io::Result<String> {
    serde_json::to_string(entries).map_err(|_| invalid("Could not encode saved snippet diagrams"))
}

fn deserialize_list<'de, D, T, const LIMIT: usize>(decoder: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct List<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> Visitor<'de> for List<T, LIMIT> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "a list with at most {LIMIT} items")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while values.len() < LIMIT {
                let Some(entry) = sequence.next_element()? else {
                    return Ok(values);
                };
                values.push(entry);
            }
            if sequence.next_element::<IgnoredAny>()?.is_some() {
                return Err(de::Error::invalid_length(LIMIT + 1, &self));
            }
            Ok(values)
        }
    }
    decoder.deserialize_seq(List::<T, LIMIT>(std::marker::PhantomData))
}

impl Portable {
    fn add_tag(&mut self, tag: String) -> io::Result<()> {
        if self.tags.len() >= MAX_TAGS {
            return Err(invalid(
                "A snippet export supports up to 128 tags per snippet",
            ));
        }
        self.tags.push(tag);
        Ok(())
    }

    fn into_input(self, id: Option<SnippetId>) -> io::Result<ImportedSnippet> {
        let content = self
            .content
            .ok_or_else(|| invalid("A snippet is missing its content"))?;
        let input = Input {
            name: self
                .name
                .ok_or_else(|| invalid("A snippet is missing its name"))?,
            language: self
                .language
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| text_file::language("", &content).into()),
            category: self.category.unwrap_or_default(),
            content,
        };
        let metadata = Metadata {
            description: self.description.unwrap_or_default(),
            operating_system: self
                .operating_system
                .filter(|value| !value.trim().is_empty()),
            folder_path: self.folder_path.unwrap_or_default(),
            file_name: self.file_name.filter(|value| !value.trim().is_empty()),
            executable: self.executable,
            tags: self.tags,
            diagrams: self.diagrams,
        };
        Ok(ImportedSnippet {
            id,
            input,
            metadata,
        })
    }
}

impl From<&Snippet> for Portable {
    fn from(entry: &Snippet) -> Self {
        let metadata = entry.metadata();
        Self {
            name: Some(entry.name().into()),
            content: Some(entry.shared_content()),
            language: Some(entry.language().into()),
            category: Some(entry.category().into()),
            description: Some(metadata.description.clone()),
            operating_system: metadata.operating_system.clone(),
            folder_path: (!metadata.folder_path.is_empty()).then(|| metadata.folder_path.clone()),
            file_name: metadata.file_name.clone(),
            executable: metadata.executable,
            tags: metadata.tags.clone(),
            diagrams: metadata.diagrams.clone(),
        }
    }
}

fn absolute(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid("Enter an absolute file path"));
    }
    Ok(())
}

fn import_files(paths: Vec<PathBuf>) -> io::Result<Imports> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(invalid("Choose 1 to 256 files to import"));
    }
    let mut imports = Imports::default();
    let mut remaining = MAX_LIBRARY_BYTES;
    for path in paths {
        absolute(&path)?;
        let format = Format::of(&path);
        let file = profile::open_preference(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("Choose regular files to import"));
        }
        let limit = if format.is_some() {
            remaining
        } else {
            remaining.min(text_file::MAX_BYTES)
        };
        if metadata.len() > limit && format.is_none() && limit == text_file::MAX_BYTES {
            imports.rejected_text += 1;
            continue;
        }
        let mut bytes = Vec::new();
        if metadata.len() <= limit {
            file.take(limit + 1).read_to_end(&mut bytes)?;
        }
        if metadata.len() > limit || bytes.len() as u64 > limit {
            return Err(invalid("The snippet import exceeds its byte limit"));
        }
        remaining -= bytes.len() as u64;
        if let (Some(format), Ok(source)) = (format, std::str::from_utf8(&bytes)) {
            let source = source.strip_prefix('\u{feff}').unwrap_or(source);
            if format.looks_like_archive(source) {
                format.import(source, &mut imports)?;
                check_count(imports.structured.len() + imports.text.len())?;
                continue;
            }
        }
        match text_file::input(&path, bytes)? {
            Some(input) => imports.text.push(input),
            None => imports.rejected_text += 1,
        }
        check_count(imports.structured.len() + imports.text.len())?;
    }
    Ok(imports)
}

fn check_count(count: usize) -> io::Result<()> {
    if count > MAX_SNIPPETS {
        return Err(invalid("A snippet import supports up to 1024 entries"));
    }
    Ok(())
}

fn read_xml(source: &str, imports: &mut Imports) -> io::Result<()> {
    let document = Document::parse_with_options(
        source,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 320_000,
            ..ParsingOptions::default()
        },
    )
    .map_err(|_| invalid("Invalid snippet XML export"))?;
    let root = document.root_element();
    if xml_is(root, "snippets") {
        return java_library::read(root, imports);
    }
    if root.tag_name().namespace().is_some()
        || !root.has_tag_name("snippetExport")
        || root
            .attribute("version")
            .is_some_and(|version| version != "1")
    {
        return Err(invalid("Expected a version 1 snippetExport element"));
    }
    for child in xml_elements(root)? {
        if !xml_is(child, "snippet") {
            return Err(invalid("Expected an XML snippet element"));
        }
        check_count(imports.structured.len() + imports.text.len() + 1)?;
        imports
            .structured
            .push(read_xml_entry(child)?.into_input(None)?);
    }
    Ok(())
}

pub(super) fn read_java_backup(bytes: &[u8]) -> io::Result<Imports> {
    let source = std::str::from_utf8(bytes).map_err(|_| invalid("Invalid snippet XML encoding"))?;
    let mut imports = Imports::default();
    read_xml(source.trim_start_matches('\u{feff}'), &mut imports)?;
    Ok(imports)
}

fn read_xml_entry(child: Node<'_, '_>) -> io::Result<Portable> {
    let mut entry = Portable::default();
    let mut seen = std::collections::HashSet::new();
    for field in xml_elements(child)? {
        let name = field.tag_name().name();
        if !xml_is(field, name) || !seen.insert(name) {
            return Err(invalid("Invalid or duplicate snippet XML field"));
        }
        if !read_xml_field(&mut entry, field)? {
            return Err(invalid("Unknown snippet XML field"));
        }
    }
    Ok(entry)
}

fn read_xml_field(entry: &mut Portable, field: Node<'_, '_>) -> io::Result<bool> {
    match field.tag_name().name() {
        "name" => entry.name = Some(xml_text(field)?),
        "content" => entry.content = Some(xml_text(field)?.into()),
        "language" => entry.language = Some(xml_text(field)?),
        "category" => entry.category = Some(xml_text(field)?),
        "description" => entry.description = Some(xml_text(field)?),
        "operatingSystem" => entry.operating_system = Some(xml_text(field)?),
        "folderPath" => entry.folder_path = Some(xml_text(field)?),
        "fileName" => entry.file_name = Some(xml_text(field)?),
        "executable" => entry.executable = Some(boolean(&xml_text(field)?)?),
        "savedDiagrams" => entry.diagrams = read_diagrams(&xml_text(field)?)?,
        "tags" => {
            for tag in xml_elements(field)? {
                if !xml_is(tag, "tag") {
                    return Err(invalid("Expected a snippet XML tag"));
                }
                entry.add_tag(xml_text(tag)?)?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn xml_text(node: Node<'_, '_>) -> io::Result<String> {
    if node
        .children()
        .any(|child| child.is_element() || child.is_pi())
    {
        return Err(invalid("Expected an XML text field"));
    }
    Ok(node
        .children()
        .filter(Node::is_text)
        .filter_map(|child| child.text())
        .collect())
}

fn boolean(value: &str) -> io::Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(
            "Expected true or false for the snippet executable flag",
        )),
    }
}

fn write_xml(entries: &[Portable], date: &str) -> io::Result<String> {
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<snippetExport version=\"1\" exportDate=\"{date}\">\n"
    );
    for entry in entries {
        output.push_str("  <snippet>\n");
        for (field, value) in [
            ("name", entry.name.as_deref()),
            ("content", entry.content.as_deref()),
            ("language", entry.language.as_deref()),
            ("category", entry.category.as_deref()),
            (
                "description",
                entry.description.as_deref().filter(|text| !text.is_empty()),
            ),
            ("operatingSystem", entry.operating_system.as_deref()),
            ("folderPath", entry.folder_path.as_deref()),
            ("fileName", entry.file_name.as_deref()),
            (
                "executable",
                entry
                    .executable
                    .map(|value| if value { "true" } else { "false" }),
            ),
        ] {
            if let Some(value) = value {
                xml_field(&mut output, "    ", field, value)?;
            }
        }
        if !entry.tags.is_empty() {
            output.push_str("    <tags>\n");
            for tag in &entry.tags {
                xml_field(&mut output, "      ", "tag", tag)?;
            }
            output.push_str("    </tags>\n");
        }
        if !entry.diagrams.is_empty() {
            xml_field(
                &mut output,
                "    ",
                "savedDiagrams",
                &write_diagrams(&entry.diagrams)?,
            )?;
        }
        output.push_str("  </snippet>\n");
    }
    output.push_str("</snippetExport>\n");
    Ok(output)
}

fn xml_field(output: &mut String, indent: &str, field: &str, value: &str) -> io::Result<()> {
    let _ = write!(output, "{indent}<{field}>");
    escape_xml(output, value)?;
    let _ = writeln!(output, "</{field}>");
    Ok(())
}

#[cfg(test)]
mod tests;
