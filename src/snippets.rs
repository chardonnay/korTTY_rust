//! Bounded native storage for the shared snippet library.

use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clipboard_policy::ClipboardPolicy;
use crate::private_files::Directory;
use crate::profile;
use crate::snippet_analysis::model::hardening::HEADER_CATEGORY;

pub(crate) mod one_liner;

const VERSION: u8 = 1;
pub(crate) const FILE_NAME: &str = "snippets.json";
const STORE_BUSY: &str = "The snippet library is being saved by another instance";
const MAX_SNIPPETS: usize = 1024;
const MAX_TAGS: usize = 128;
const MAX_VARIABLES: usize = 256;
const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_VARIABLE_NAME_BYTES: usize = 128;
pub(crate) const MAX_VARIABLE_VALUE_BYTES: usize = 4096;
// A 5 MiB Java text import may double in size when converting Latin-1 to UTF-8.
pub(crate) const MAX_CONTENT_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_NAME_BYTES: usize = 160;
pub(crate) const MAX_CATEGORY_BYTES: usize = 96;
pub(crate) const MAX_LANGUAGE_BYTES: usize = 64;
pub(crate) const MAX_OPERATING_SYSTEM_BYTES: usize = 128;

pub(crate) mod archive;
pub(crate) mod diagrams;
pub(crate) mod exchange;
pub(crate) mod folders;
pub(crate) mod modularization;
pub(crate) mod recipients;
pub(crate) mod resolve;

pub(crate) fn profile_path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join(FILE_NAME))
}

pub(crate) fn is_script_header_category(category: &str) -> bool {
    category.trim().eq_ignore_ascii_case(HEADER_CATEGORY)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnippetId([u8; 16]);

impl SnippetId {
    pub(crate) fn new() -> io::Result<Self> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        Ok(Self(bytes))
    }

    pub(super) fn from_java_uuid(text: &str) -> io::Result<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 36
            || ![8, 13, 18, 23]
                .into_iter()
                .all(|index| bytes[index] == b'-')
        {
            return Err(invalid("Invalid Java snippet library UUID"));
        }
        Self::try_from(text.replace('-', ""))
            .map_err(|_| invalid("Invalid Java snippet library UUID"))
    }
}

impl TryFrom<String> for SnippetId {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let mut bytes = [0; 16];
        if text.len() != bytes.len() * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Invalid snippet ID");
        }
        for (index, value) in bytes.iter_mut().enumerate() {
            *value = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(|_| "Invalid snippet ID")?;
        }
        Ok(Self(bytes))
    }
}

impl From<SnippetId> for String {
    fn from(id: SnippetId) -> Self {
        let mut text = Self::with_capacity(32);
        for byte in id.0 {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snippet {
    id: SnippetId,
    name: String,
    language: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    category: String,
    content: Arc<str>,
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    metadata: Metadata,
}

impl Snippet {
    pub(crate) fn id(&self) -> SnippetId {
        self.id
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn language(&self) -> &str {
        &self.language
    }
    pub(crate) fn category(&self) -> &str {
        &self.category
    }

    fn is_script_header(&self) -> bool {
        is_script_header_category(&self.category)
    }

    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    pub(crate) fn shared_content(&self) -> Arc<str> {
        Arc::clone(&self.content)
    }

    pub(crate) fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn from_input(id: SnippetId, input: Input, metadata: Metadata) -> Self {
        Self {
            id,
            name: input.name.trim().into(),
            language: input.language.trim().into(),
            category: input.category.trim().into(),
            content: input.content,
            metadata,
        }
    }

    pub(crate) fn label(&self) -> String {
        if self.category.is_empty() {
            format!("{} · {}", self.name, self.language)
        } else {
            format!("{} · {} · {}", self.name, self.category, self.language)
        }
    }

    fn validate(&self) -> io::Result<()> {
        if !valid_field(&self.name, MAX_NAME_BYTES) || self.name.is_empty() {
            return Err(invalid("Enter a snippet name up to 160 bytes"));
        }
        if !valid_field(&self.language, MAX_LANGUAGE_BYTES) || self.language.is_empty() {
            return Err(invalid("Enter a code language up to 64 bytes"));
        }
        if !valid_field(&self.category, MAX_CATEGORY_BYTES) {
            return Err(invalid("Enter a category up to 96 bytes"));
        }
        if self.content.len() > MAX_CONTENT_BYTES || self.content.contains('\0') {
            return Err(invalid("Snippet content must be UTF-8 text up to 10 MiB"));
        }
        self.metadata.validate()
    }
}

impl fmt::Debug for Snippet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snippet")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn valid_field(text: &str, limit: usize) -> bool {
    text.trim() == text && text.len() <= limit && !text.chars().any(char::is_control)
}

#[derive(Clone)]
pub(crate) struct Input {
    pub name: String,
    pub language: String,
    pub category: String,
    pub content: Arc<str>,
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Metadata {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) operating_system: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) folder_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) executable: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagrams: Vec<diagrams::Saved>,
}

impl Metadata {
    fn is_empty(&self) -> bool {
        self.description.is_empty()
            && self.operating_system.is_none()
            && self.folder_path.is_empty()
            && self.file_name.is_none()
            && self.executable.is_none()
            && self.tags.is_empty()
            && self.diagrams.is_empty()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.description.len() > 16 * 1024
            || self.description.contains('\0')
            || self.operating_system.as_deref().is_some_and(|system| {
                system.is_empty()
                    || !valid_field(system, MAX_OPERATING_SYSTEM_BYTES)
                    || !system
                        .chars()
                        .all(crate::session_journal::is_display_character)
            })
            || !valid_field(&self.folder_path, 1024)
            || self
                .file_name
                .as_deref()
                .is_some_and(|name| !valid_field(name, 255))
            || self.tags.len() > MAX_TAGS
            || self.tags.iter().any(|tag| !valid_field(tag, 128))
        {
            return Err(invalid(
                "Snippet description, operating system, folder, filename or tags exceed the metadata limits",
            ));
        }
        diagrams::validate_collection(&self.diagrams)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Variable {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) value: String,
}

impl Variable {
    pub(crate) fn matches(&self, name: &str) -> bool {
        same_variable(&self.name, name)
    }

    pub(crate) fn default_value(&self) -> Option<&str> {
        (!self.value.trim().is_empty()).then_some(&self.value)
    }

    fn validate(&self) -> io::Result<()> {
        if !valid_field(&self.name, MAX_VARIABLE_NAME_BYTES)
            || self.name.is_empty()
            || self.name.contains(['{', '}'])
        {
            return Err(invalid(
                "Enter a variable name up to 128 bytes, without braces",
            ));
        }
        if !valid_variable_value(&self.value) {
            return Err(invalid("Enter a text value of at most 4 KiB"));
        }
        Ok(())
    }
}

pub(crate) fn valid_variable_value(value: &str) -> bool {
    value.len() <= MAX_VARIABLE_VALUE_BYTES && !value.contains('\0')
}

fn same_variable(first: &str, second: &str) -> bool {
    if first.is_ascii() && second.is_ascii() {
        first.eq_ignore_ascii_case(second)
    } else {
        first.to_lowercase() == second.to_lowercase()
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    snippets: Vec<Snippet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    folders: Vec<folders::Folder>,
    #[serde(skip)]
    folder_catalog: Arc<folders::Catalog>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            snippets: Vec::new(),
            variables: Vec::new(),
            folders: Vec::new(),
            folder_catalog: Arc::default(),
        }
    }
}

impl Document {
    fn adds_script_headers(&self, previous: &Self) -> bool {
        let mut headers = self
            .snippets
            .iter()
            .filter(|entry| entry.is_script_header())
            .peekable();
        if headers.peek().is_none() {
            return false;
        }
        let existing: HashSet<_> = previous
            .snippets
            .iter()
            .filter(|entry| entry.is_script_header())
            .map(Snippet::id)
            .collect();
        headers.any(|entry| !existing.contains(&entry.id()))
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.snippets.len() > MAX_SNIPPETS {
            return Err(invalid(
                "Unsupported snippet library version or entry count",
            ));
        }
        let mut ids = HashSet::with_capacity(self.snippets.len());
        let mut names = HashSet::with_capacity(self.snippets.len());
        for snippet in &self.snippets {
            snippet.validate()?;
            if !ids.insert(snippet.id) || !names.insert(snippet.name.to_lowercase()) {
                return Err(invalid("Snippet names and IDs must be unique"));
            }
        }
        if self.variables.len() > MAX_VARIABLES {
            return Err(invalid("The snippet library supports up to 256 variables"));
        }
        let mut names = HashSet::with_capacity(self.variables.len());
        for variable in &self.variables {
            variable.validate()?;
            if !names.insert(variable.name.to_lowercase()) {
                return Err(invalid("Variable names must be unique, ignoring case"));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Library {
    path: PathBuf,
    document: Arc<Document>,
    #[cfg(test)]
    allow_custom_script_headers: Option<bool>,
}

impl Library {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        let document = Arc::new(read_document(&path)?);
        Ok(Self {
            path,
            document,
            #[cfg(test)]
            allow_custom_script_headers: None,
        })
    }

    pub(crate) fn shares_store(&self, other: &Self) -> bool {
        self.path == other.path
    }

    pub(crate) fn same_revision(&self, other: &Self) -> bool {
        self.shares_store(other)
            && (Arc::ptr_eq(&self.document, &other.document) || self.document == other.document)
    }

    pub(crate) fn entries(&self) -> &[Snippet] {
        &self.document.snippets
    }

    pub(crate) fn get(&self, id: SnippetId) -> Option<&Snippet> {
        self.entries().iter().find(|entry| entry.id == id)
    }

    pub(crate) fn variables(&self) -> &[Variable] {
        &self.document.variables
    }

    pub(crate) fn remember_missing_defaults<'a>(
        &mut self,
        selected: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> io::Result<bool> {
        let mut selected = selected
            .filter(|(_, value)| !value.trim().is_empty())
            .peekable();
        if selected.peek().is_none() {
            return Ok(false);
        }
        let mut candidate = self.document.as_ref().clone();
        for (name, value) in selected {
            if !valid_variable_value(value) {
                return Err(invalid("Enter a text value of at most 4 KiB"));
            }
            let variable = candidate
                .variables
                .iter_mut()
                .find(|variable| variable.matches(name) && variable.default_value().is_none())
                .ok_or_else(|| invalid("The variable no longer needs a stored default"))?;
            variable.value = value.to_owned();
        }
        self.commit(candidate)?;
        Ok(true)
    }

    pub(crate) fn save_variable(
        &mut self,
        editing: Option<&str>,
        mut variable: Variable,
    ) -> io::Result<()> {
        variable.name = variable.name.trim().to_owned();
        let mut candidate = self.document.as_ref().clone();
        if let Some(name) = editing {
            let entry = candidate
                .variables
                .iter_mut()
                .find(|entry| entry.matches(name))
                .ok_or_else(|| invalid("The variable no longer exists"))?;
            *entry = variable;
        } else {
            candidate.variables.push(variable);
        }
        candidate
            .variables
            .sort_by_cached_key(|entry| entry.name.to_lowercase());
        self.commit(candidate)
    }

    pub(crate) fn import_variables(
        &mut self,
        incoming: Vec<Variable>,
        replace_existing: bool,
    ) -> io::Result<usize> {
        let mut candidate = self.document.as_ref().clone();
        let mut applied = 0;
        for variable in incoming {
            match candidate
                .variables
                .iter_mut()
                .find(|entry| entry.matches(&variable.name))
            {
                Some(entry) if replace_existing => entry.value = variable.value,
                Some(_) => continue,
                None => candidate.variables.push(variable),
            }
            applied += 1;
        }
        candidate
            .variables
            .sort_by_cached_key(|entry| entry.name.to_lowercase());
        if applied > 0 {
            self.commit(candidate)?;
        }
        Ok(applied)
    }

    pub(crate) fn remove_variable(&mut self, name: &str) -> io::Result<()> {
        let mut candidate = self.document.as_ref().clone();
        let index = candidate
            .variables
            .iter()
            .position(|entry| entry.matches(name))
            .ok_or_else(|| invalid("The variable no longer exists"))?;
        candidate.variables.remove(index);
        self.commit(candidate)
    }

    pub(crate) fn reload(&mut self) -> io::Result<()> {
        self.document = Arc::new(read_document(&self.path)?);
        Ok(())
    }

    pub(crate) fn save(
        &mut self,
        editing: Option<SnippetId>,
        input: Input,
    ) -> io::Result<SnippetId> {
        self.save_with_metadata(editing, input, None)
    }

    pub(crate) fn replace_contents<'a>(
        &mut self,
        changes: impl IntoIterator<Item = (SnippetId, &'a str, &'a str)>,
    ) -> io::Result<()> {
        let mut candidate = self.document.as_ref().clone();
        let mut replaced = HashSet::new();
        for (id, before, after) in changes {
            if !replaced.insert(id) {
                return Err(invalid("A reviewed snippet was selected more than once"));
            }
            let snippet = candidate
                .snippets
                .iter_mut()
                .find(|snippet| snippet.id == id && snippet.content() == before)
                .ok_or_else(|| invalid("A reviewed snippet changed before saving"))?;
            if after.trim().is_empty() {
                return Err(invalid("Enter snippet text"));
            }
            diagrams::rebase(&mut snippet.metadata.diagrams, before, after);
            snippet.content = after.into();
        }
        if replaced.is_empty() {
            return Ok(());
        }
        self.commit(candidate)
    }

    pub(crate) fn save_with_metadata(
        &mut self,
        editing: Option<SnippetId>,
        input: Input,
        metadata: Option<Metadata>,
    ) -> io::Result<SnippetId> {
        self.store_snippet(editing, None, input, metadata)
    }

    pub(crate) fn create_with_id(
        &mut self,
        id: SnippetId,
        input: Input,
        metadata: Metadata,
    ) -> io::Result<SnippetId> {
        if self.get(id).is_some() {
            return Err(invalid("The new snippet ID is already in use"));
        }
        self.store_snippet(None, Some(id), input, Some(metadata))
    }

    fn store_snippet(
        &mut self,
        editing: Option<SnippetId>,
        creating: Option<SnippetId>,
        input: Input,
        metadata: Option<Metadata>,
    ) -> io::Result<SnippetId> {
        let mut candidate = self.document.as_ref().clone();
        let id = editing.or(creating).map_or_else(SnippetId::new, Ok)?;
        if editing.is_some() {
            if let (Some(previous), Some(metadata)) = (self.get(id), metadata.as_ref())
                && previous.metadata.folder_path != metadata.folder_path
            {
                candidate.preserve_folder(&previous.metadata.folder_path);
            }
            let previous = candidate
                .snippets
                .iter_mut()
                .find(|snippet| snippet.id == id)
                .ok_or_else(|| invalid("The snippet no longer exists"))?;
            if input.content.trim().is_empty() && !previous.content.trim().is_empty() {
                return Err(invalid("Enter snippet text"));
            }
            let mut metadata = match metadata {
                Some(mut metadata) => {
                    // Editor details and recovered drafts do not own diagram changes.
                    metadata.diagrams.clone_from(&previous.metadata.diagrams);
                    metadata
                }
                None => previous.metadata.clone(),
            };
            diagrams::rebase(&mut metadata.diagrams, &previous.content, &input.content);
            *previous = Snippet::from_input(id, input, metadata);
        } else {
            if input.content.trim().is_empty() {
                return Err(invalid("Enter snippet text"));
            }
            candidate
                .snippets
                .push(Snippet::from_input(id, input, metadata.unwrap_or_default()));
        }
        self.commit(candidate)?;
        Ok(id)
    }

    pub(crate) fn import_snippets(
        &mut self,
        imports: archive::Imports,
    ) -> io::Result<Option<SnippetId>> {
        let mut candidate = self.document.as_ref().clone();
        let folders = candidate.import_java_folders(imports.folders)?;
        let mut names: HashSet<_> = candidate
            .snippets
            .iter()
            .map(|entry| entry.name.to_lowercase())
            .collect();
        let start = candidate.snippets.len();
        for archive::ImportedSnippet {
            id,
            input,
            metadata,
        } in imports.structured
        {
            if !names.insert(input.name.trim().to_lowercase()) {
                return Err(invalid("An imported snippet name is already in use"));
            }
            let id = match id {
                Some(id) => id,
                None => SnippetId::new()?,
            };
            candidate
                .snippets
                .push(Snippet::from_input(id, input, metadata));
        }
        for mut input in imports.text {
            let base = input.name.trim();
            let mut name = base.to_owned();
            for number in 2..=MAX_SNIPPETS + 1 {
                if !names.contains(&name.to_lowercase()) {
                    break;
                }
                let suffix = format!(" ({number})");
                let prefix =
                    base.floor_char_boundary((MAX_NAME_BYTES - suffix.len()).min(base.len()));
                name = format!("{}{suffix}", &base[..prefix]);
            }
            if !names.insert(name.to_lowercase()) {
                return Err(invalid(
                    "Could not assign a unique name to an imported text file",
                ));
            }
            input.name = name;
            candidate.snippets.push(Snippet::from_input(
                SnippetId::new()?,
                input,
                Metadata::default(),
            ));
        }
        let first = candidate.snippets.get(start).map(Snippet::id);
        if first.is_some() || folders {
            self.commit(candidate)?;
        }
        Ok(first)
    }

    pub(crate) fn remove(&mut self, id: SnippetId) -> io::Result<()> {
        let mut candidate = self.document.as_ref().clone();
        let index = candidate
            .snippets
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| invalid("The snippet no longer exists"))?;
        candidate.preserve_folder(&self.entries()[index].metadata.folder_path);
        candidate.snippets.remove(index);
        self.commit(candidate)
    }

    fn commit(&mut self, mut candidate: Document) -> io::Result<()> {
        candidate.validate()?;
        candidate.materialize_folders()?;
        candidate.preserve_displaced_folders()?;
        let bytes = serde_json::to_vec(&candidate)
            .map_err(|_| invalid("Could not encode snippet library"))?;
        if bytes.len() as u64 > MAX_LIBRARY_BYTES {
            return Err(invalid("The snippet library exceeds 64 MiB"));
        }
        let _lock = StoreLock::acquire(&self.path)?;
        let previous = read_document(&self.path)?;
        if previous != *self.document {
            return Err(io::Error::other(
                "The snippet library changed. Reload before saving again.",
            ));
        }
        if candidate.adds_script_headers(&previous) {
            #[cfg(not(test))]
            let allowed = custom_script_headers_allowed();
            #[cfg(test)]
            let allowed = self
                .allow_custom_script_headers
                .unwrap_or_else(custom_script_headers_allowed);
            if !allowed {
                return Err(header_creation_denied());
            }
        }
        profile::save_atomic(&self.path, &bytes)?;
        self.document = Arc::new(candidate);
        Ok(())
    }
}

fn read_document(path: &Path) -> io::Result<Document> {
    let file = match profile::open_preference(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Document::default()),
        Err(error) => return Err(error),
    };
    read_document_file(file)
}

fn read_document_file(file: File) -> io::Result<Document> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES {
        return Err(invalid(
            "The snippet library must be a regular file of at most 64 MiB",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The snippet library must be private to the current user",
            ));
        }
    }
    let mut bytes = Vec::new();
    file.take(MAX_LIBRARY_BYTES + 1).read_to_end(&mut bytes)?;
    decode_document(&bytes)
}

pub(crate) fn custom_script_headers_allowed() -> bool {
    let policy = ClipboardPolicy::load();
    policy.problem.is_none() && policy.snippets.allow_custom_script_headers
}

pub(crate) fn check_restore_headers(
    previous: Option<&Directory>,
    incoming: &[u8],
) -> io::Result<()> {
    let candidate = decode_document(incoming)?;
    if !candidate.snippets.iter().any(Snippet::is_script_header) {
        return Ok(());
    }
    let previous = if let Some(directory) = previous {
        match directory.open_file(FILE_NAME).and_then(read_document_file) {
            Ok(document) => document,
            // A missing or corrupt library cannot establish an existing header identity.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                Document::default()
            }
            Err(error) => return Err(error),
        }
    } else {
        Document::default()
    };
    if candidate.adds_script_headers(&previous) {
        Err(header_creation_denied())
    } else {
        Ok(())
    }
}

fn header_creation_denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Managed policy blocks creating custom script headers.",
    )
}

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode_document(bytes).map(|_| ())
}

/// Identifies executable Java snippet links that survived an additive merge.
pub(crate) fn java_scheduler_snippets(
    incoming: &[u8],
    merged: &[u8],
) -> io::Result<HashSet<SnippetId>> {
    use crate::scheduler_window::script::{Language, MAX_COMMAND_BYTES};

    let incoming = decode_document(incoming)?;
    let merged = decode_document(merged)?;
    let by_id: std::collections::HashMap<_, _> = merged
        .snippets
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    Ok(incoming
        .snippets
        .iter()
        .filter(|source| {
            let Some(saved) = by_id.get(&source.id) else {
                return false;
            };
            !source.is_script_header()
                && !saved.is_script_header()
                && saved.content == source.content
                && saved.language == source.language
                && Language::from_snippet(&source.language).is_some()
                && source.content.len() <= MAX_COMMAND_BYTES
                && resolve::referenced_variables(&source.content, &incoming.variables)
                    == resolve::referenced_variables(&saved.content, &merged.variables)
                && resolve::headless(&saved.content, &merged.variables, MAX_COMMAND_BYTES).is_ok()
        })
        .map(Snippet::id)
        .collect())
}

pub(crate) fn portable_java_backup(
    snippets: Option<&[u8]>,
    variables: Option<&[u8]>,
) -> io::Result<(Vec<u8>, usize, usize, usize, usize)> {
    let imports = snippets
        .map(archive::read_java_backup)
        .transpose()?
        .unwrap_or_default();
    let skipped_diagrams = imports.skipped_diagrams;
    let skipped_references = imports.skipped_references;
    let mut document = Document::default();
    document.import_java_folders(imports.folders)?;
    let mut skipped_headers = 0;
    for archive::ImportedSnippet {
        id,
        input,
        metadata,
    } in imports.structured
    {
        let id = id.map(Ok).unwrap_or_else(SnippetId::new)?;
        let snippet = Snippet::from_input(id, input, metadata);
        if snippet.is_script_header() && !custom_script_headers_allowed() {
            skipped_headers += 1;
        } else {
            document.snippets.push(snippet);
        }
    }
    document.variables = variables
        .map(exchange::read_java_backup)
        .transpose()?
        .unwrap_or_default();
    let count = document.snippets.len() + document.folders.len() + document.variables.len();
    Ok((
        encode_java_backup(document)?,
        count,
        skipped_headers,
        skipped_diagrams,
        skipped_references,
    ))
}

pub(crate) fn merge_java_backup(
    local: Option<&[u8]>,
    incoming: &[u8],
    allow_headers: bool,
) -> io::Result<Option<Vec<u8>>> {
    let mut target = local.map(decode_document).transpose()?.unwrap_or_default();
    let incoming = decode_document(incoming)?;
    let mut names: HashSet<_> = target
        .snippets
        .iter()
        .map(|entry| entry.name.to_lowercase())
        .collect();
    let mut ids: HashSet<_> = target.snippets.iter().map(Snippet::id).collect();
    let before = (
        target.snippets.len(),
        target.variables.len(),
        target.folders.len(),
    );
    target.merge_java_backup_folders(incoming.folders);
    for snippet in incoming.snippets {
        if snippet.is_script_header() && !allow_headers {
            continue;
        }
        let name = snippet.name.to_lowercase();
        if !names.contains(&name) && !ids.contains(&snippet.id) {
            names.insert(name);
            ids.insert(snippet.id);
            target.snippets.push(snippet);
        }
    }
    for variable in incoming.variables {
        if !target
            .variables
            .iter()
            .any(|entry| entry.matches(&variable.name))
        {
            target.variables.push(variable);
        }
    }
    if before
        == (
            target.snippets.len(),
            target.variables.len(),
            target.folders.len(),
        )
    {
        return Ok(None);
    }
    target
        .variables
        .sort_by_cached_key(|entry| entry.name.to_lowercase());
    encode_java_backup(target).map(Some)
}

fn encode_java_backup(mut document: Document) -> io::Result<Vec<u8>> {
    document.validate()?;
    document.materialize_folders()?;
    document.preserve_displaced_folders()?;
    let bytes =
        serde_json::to_vec(&document).map_err(|_| invalid("Could not encode snippet library"))?;
    if bytes.len() as u64 > MAX_LIBRARY_BYTES {
        return Err(invalid("The snippet library exceeds 64 MiB"));
    }
    Ok(bytes)
}

fn decode_document(bytes: &[u8]) -> io::Result<Document> {
    if bytes.len() as u64 > MAX_LIBRARY_BYTES {
        return Err(invalid("The snippet library exceeds 64 MiB"));
    }
    let mut document: Document =
        serde_json::from_slice(bytes).map_err(|_| invalid("Invalid snippet library JSON"))?;
    document.validate()?;
    document.materialize_folders()?;
    Ok(document)
}

pub(crate) struct StoreLock(File);

impl StoreLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        Self::acquire_for(path, STORE_BUSY)
    }

    pub(crate) fn acquire_for_restore(root: &Path) -> io::Result<Self> {
        Self::acquire(&root.join(FILE_NAME))
    }

    fn acquire_for(path: &Path, busy: &'static str) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("The store has no profile directory"))?;
        fs::create_dir_all(parent)?;
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let file = open_lock(Path::new(&lock_path))?;
        if !file.metadata()?.is_file() {
            return Err(invalid("The store lock must be a regular file"));
        }
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::other(busy),
            TryLockError::Error(error) => error,
        })?;
        Ok(Self(file))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(unix)]
fn open_lock(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let flags =
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    Ok(File::from(open(path, flags, Mode::RUSR | Mode::WUSR)?))
}

#[cfg(windows)]
fn open_lock(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    // CreateFileW's FILE_FLAG_OPEN_REPARSE_POINT prevents resolving the final component.
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)?;
    if kortty_files::tree::is_reparse_point(&file.metadata()?) {
        return Err(invalid("The store lock cannot be a reparse point"));
    }
    Ok(file)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
pub(crate) mod tests;
