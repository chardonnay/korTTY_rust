//! Bounded native storage for the shared snippet library.

use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::profile;

const VERSION: u8 = 1;
pub(crate) const FILE_NAME: &str = "snippets.json";
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

pub(crate) mod archive;
pub(crate) mod exchange;
pub(crate) mod folders;
pub(crate) mod recipients;
pub(crate) mod resolve;

pub(crate) fn profile_path() -> io::Result<PathBuf> {
    Ok(profile::root()?.join(FILE_NAME))
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) folder_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) executable: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tags: Vec<String>,
}

impl Metadata {
    fn is_empty(&self) -> bool {
        self.description.is_empty()
            && self.folder_path.is_empty()
            && self.file_name.is_none()
            && self.executable.is_none()
            && self.tags.is_empty()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.description.len() > 16 * 1024
            || self.description.contains('\0')
            || !valid_field(&self.folder_path, 1024)
            || self
                .file_name
                .as_deref()
                .is_some_and(|name| !valid_field(name, 255))
            || self.tags.len() > MAX_TAGS
            || self.tags.iter().any(|tag| !valid_field(tag, 128))
        {
            return Err(invalid(
                "Snippet description, folder, filename or tags exceed the metadata limits",
            ));
        }
        Ok(())
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
}

impl Library {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        let document = Arc::new(read_document(&path)?);
        Ok(Self { path, document })
    }

    pub(crate) fn shares_store(&self, other: &Self) -> bool {
        self.path == other.path
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
            let metadata = metadata.unwrap_or_else(|| previous.metadata.clone());
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
        let mut names: HashSet<_> = candidate
            .snippets
            .iter()
            .map(|entry| entry.name.to_lowercase())
            .collect();
        let start = candidate.snippets.len();
        for (input, metadata) in imports.structured {
            if !names.insert(input.name.trim().to_lowercase()) {
                return Err(invalid("An imported snippet name is already in use"));
            }
            candidate
                .snippets
                .push(Snippet::from_input(SnippetId::new()?, input, metadata));
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
        if first.is_some() {
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
        if read_document(&self.path)? != *self.document {
            return Err(io::Error::other(
                "The snippet library changed. Reload before saving again.",
            ));
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

pub(crate) fn validate_backup(bytes: &[u8]) -> io::Result<()> {
    decode_document(bytes).map(|_| ())
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

struct StoreLock(File);

impl StoreLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        Self::acquire_for(
            path,
            "The snippet library is being saved by another instance",
        )
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
