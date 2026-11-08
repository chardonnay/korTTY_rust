use std::collections::{HashMap, HashSet};
use std::io;

use unicode_normalization::UnicodeNormalization as _;

use crate::snippet_transfer::{Entry as TransferEntry, Layout as TransferLayout};
use crate::snippets::{Library, SnippetId, folders};

use super::super::{FolderSelection, text_file};
use super::{Snippet, Source, file_content, invalid};

const MAX_COMPONENT_BYTES: usize = 255;
const MAX_DEPTH: usize = 32;

pub(super) struct Entry<'a> {
    pub(super) path: String,
    pub(super) snippet: &'a Snippet,
    pub(super) mode: u32,
}

pub(super) struct Layout<'a> {
    pub(super) directories: Vec<String>,
    pub(super) entries: Vec<Entry<'a>>,
}

impl<'a> Layout<'a> {
    pub(super) fn build(source: Source<'a>, forced: Option<&str>) -> io::Result<Self> {
        Self::build_entries(source.entries(), source.directories(), forced)
    }

    fn build_entries(
        mut ordered: Vec<(&'a Snippet, &str)>,
        extra_directories: &[String],
        forced: Option<&str>,
    ) -> io::Result<Self> {
        if ordered.is_empty() && extra_directories.is_empty() {
            return Err(invalid("Select snippets to export"));
        }
        if forced.is_none() {
            ordered
                .sort_by_cached_key(|(snippet, directory)| (key(directory), key(snippet.name())));
        }
        let mapped = map_folders(extra_directories)?;
        let mut directories = HashMap::<String, String>::new();
        let mut parents = Vec::with_capacity(ordered.len());
        for (_, directory) in &ordered {
            parents.push(if forced.is_some() || directory.is_empty() {
                String::new()
            } else if mapped.is_empty() {
                folder_path(directory, &mut directories)?
            } else {
                mapped
                    .get(directory)
                    .cloned()
                    .ok_or_else(|| invalid("A snippet refers to a directory outside the export"))?
            });
        }
        let mut used: HashSet<_> = directories
            .keys()
            .cloned()
            .chain(mapped.values().map(|path| key(path)))
            .collect();
        let mut entries = Vec::with_capacity(ordered.len());
        for ((index, (snippet, _)), parent) in ordered.into_iter().enumerate().zip(parents) {
            let file = forced.map_or_else(
                || file_name(snippet),
                |extension| forced_name(snippet, index, extension),
            );
            let file = protect_device_name(file);
            let mode = mode(snippet, &file);
            let path = unique_path(&parent, &file, forced.is_some(), &mut used)?;
            entries.push(Entry {
                path,
                snippet,
                mode,
            });
        }
        let mut directories: Vec<_> = directories
            .into_values()
            .chain(mapped.into_values())
            .collect();
        directories
            .sort_by_cached_key(|directory| (directory.matches('/').count(), key(directory)));
        Ok(Self {
            directories,
            entries,
        })
    }

    fn into_transfer(self) -> TransferLayout {
        TransferLayout {
            directories: self.directories,
            entries: self
                .entries
                .into_iter()
                .map(|entry| TransferEntry {
                    path: entry.path,
                    content: file_content(entry.snippet, true).into_owned(),
                    mode: entry.mode,
                })
                .collect(),
        }
    }
}

impl TransferLayout {
    /// Captures the selected folder itself and every descendant for transfer.
    pub(crate) fn folder(library: &Library, id: folders::Id) -> io::Result<Self> {
        let selection = FolderSelection::new(Some(id), library)?;
        Ok(Layout::build(Source::Folder(&selection), None)?.into_transfer())
    }

    /// Captures the requested files relative to the folder currently shown in the manager.
    /// Files outside that folder are placed at the transfer root.
    pub(crate) fn snippets(
        library: &Library,
        ids: &[SnippetId],
        scope: Option<folders::Id>,
    ) -> io::Result<Self> {
        Ok(Layout::selected(library, ids, scope)?.into_transfer())
    }
}

pub(crate) struct AnalysisFile<'a> {
    pub(crate) path: String,
    pub(crate) snippet: &'a Snippet,
    pub(crate) executable: bool,
}

pub(crate) fn analysis_files<'a>(
    library: &'a Library,
    ids: &[SnippetId],
    scope: Option<folders::Id>,
) -> io::Result<Vec<AnalysisFile<'a>>> {
    Ok(Layout::selected(library, ids, scope)?
        .entries
        .into_iter()
        .map(|entry| AnalysisFile {
            path: entry.path,
            snippet: entry.snippet,
            executable: entry.mode == 0o755,
        })
        .collect())
}

impl<'a> Layout<'a> {
    fn selected(
        library: &'a Library,
        ids: &[SnippetId],
        scope: Option<folders::Id>,
    ) -> io::Result<Self> {
        let prefix = if let Some(id) = scope {
            format!(
                "{}/",
                library
                    .folder(id)
                    .ok_or_else(|| invalid("The snippet folder no longer exists"))?
                    .path
            )
        } else {
            String::new()
        };
        let ids: HashSet<_> = ids.iter().copied().collect();
        let mut selected = Vec::with_capacity(ids.len());
        let mut directories = HashSet::new();
        for snippet in library
            .entries()
            .iter()
            .filter(|entry| ids.contains(&entry.id()))
        {
            let directory = library
                .folder_id(snippet)
                .and_then(|id| library.folder(id))
                .and_then(|folder| folder.path.strip_prefix(prefix.as_str()))
                .unwrap_or_default();
            selected.push((snippet, directory));

            let mut parent = directory;
            while !parent.is_empty() && directories.insert(parent) {
                parent = parent.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
        if selected.len() != ids.len() {
            return Err(invalid("A selected snippet no longer exists"));
        }
        let directories: Vec<_> = directories.into_iter().map(str::to_owned).collect();
        Self::build_entries(selected, &directories, None)
    }
}

fn map_folders(directories: &[String]) -> io::Result<HashMap<&str, String>> {
    let mut ordered: Vec<_> = directories.iter().map(String::as_str).collect();
    ordered.sort_by_cached_key(|path| (path.matches('/').count(), *path));
    let mut mapped = HashMap::<&str, String>::with_capacity(ordered.len());
    let mut used = HashSet::with_capacity(ordered.len());
    for path in ordered {
        let (parent, name) = match path.rsplit_once('/') {
            Some((parent, name)) => (
                mapped
                    .get(parent)
                    .ok_or_else(|| invalid("A snippet export directory is missing its parent"))?
                    .as_str(),
                name,
            ),
            None => ("", path),
        };
        let segment = folder_segment(name)
            .filter(|segment| segment.len() <= MAX_COMPONENT_BYTES)
            .ok_or_else(|| invalid("A snippet export directory has an invalid name"))?;
        if path.matches('/').count() >= MAX_DEPTH {
            return Err(invalid("The snippet folder path is too deep"));
        }
        // Preserve different source folders when sanitizing a name would merge them.
        let destination = unique_path(parent, &segment, false, &mut used)?;
        mapped.insert(path, destination);
    }
    Ok(mapped)
}

fn folder_path(path: &str, directories: &mut HashMap<String, String>) -> io::Result<String> {
    let mut parent = String::new();
    for (index, segment) in path
        .split(['/', '\\'])
        .filter_map(folder_segment)
        .enumerate()
    {
        if index >= MAX_DEPTH || segment.len() > MAX_COMPONENT_BYTES {
            return Err(invalid(
                "The snippet folder path is too deep or contains an oversized segment",
            ));
        }
        let path = relative(&parent, &segment);
        parent = directories.entry(key(&path)).or_insert(path).clone();
    }
    Ok(parent)
}

fn folder_segment(segment: &str) -> Option<String> {
    let clean: String = segment
        .trim()
        .chars()
        .map(|character| {
            if invalid_path_char(character) {
                '-'
            } else {
                character
            }
        })
        .collect();
    let clean = clean.trim_end_matches(['.', ' ']).trim();
    (!clean.is_empty()).then(|| protect_device_name(clean.into()))
}

fn key(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).nfc().collect()
}

fn relative(parent: &str, file: &str) -> String {
    if parent.is_empty() {
        file.into()
    } else {
        format!("{parent}/{file}")
    }
}

fn unique_path(
    parent: &str,
    file: &str,
    flat: bool,
    used: &mut HashSet<String>,
) -> io::Result<String> {
    let (stem, extension) = split_extension(file);
    for index in 1..=super::super::MAX_SNIPPETS + used.len() + 1 {
        let suffix = match (index, flat) {
            (1, _) => String::new(),
            (index, true) => format!(" ({index})"),
            (index, false) => format!("-{index}"),
        };
        let available = MAX_COMPONENT_BYTES.saturating_sub(extension.len() + suffix.len());
        let end = stem.floor_char_boundary(stem.len().min(available));
        if end == 0 {
            return Err(invalid("The snippet filename extension is too long"));
        }
        let candidate = relative(parent, &format!("{}{suffix}{extension}", &stem[..end]));
        if used.insert(key(&candidate)) {
            return Ok(candidate);
        }
    }
    Err(invalid("Could not assign a unique snippet export filename"))
}

fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => name.split_at(index),
        _ => (name, ""),
    }
}

fn file_name(snippet: &Snippet) -> String {
    if let Some(name) = &snippet.metadata().file_name {
        let name = ascii_filename(name, false);
        return if name.is_empty() {
            "snippet.txt".into()
        } else {
            name
        };
    }
    let language = snippet.language().trim().to_ascii_lowercase();
    let language = match language.as_str() {
        "plain" | "text" | "txt" => text_file::language("", snippet.content()),
        language => language,
    };
    if language == "plain" {
        let name = ascii_filename(snippet.name(), false);
        if name.is_empty() {
            return "snippet.txt".into();
        }
        if !split_extension(&name).1.is_empty() {
            return name;
        }
        return format!("{name}.txt");
    }
    let extension = default_extension(language);
    let name = ascii_filename(snippet.name(), true);
    let name = if name.is_empty() { "snippet" } else { &name };
    if name
        .to_ascii_lowercase()
        .ends_with(&extension.to_ascii_lowercase())
    {
        return name.into();
    }
    let (stem, existing) = split_extension(name);
    let base = if !existing.is_empty()
        && existing[1..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        stem
    } else {
        name
    };
    format!("{base}{extension}")
}

fn ascii_filename(name: &str, generated: bool) -> String {
    let mut output = String::new();
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_') {
            output.push(character);
        } else if !output.ends_with('-') {
            output.push('-');
        }
    }
    if generated {
        output.trim_matches(['.', '_', '-']).into()
    } else {
        output
            .trim_start_matches(['.', '-'])
            .trim_end_matches(['.', '-', ' '])
            .into()
    }
}

fn forced_name(snippet: &Snippet, index: usize, extension: &str) -> String {
    let name: String = snippet
        .name()
        .trim()
        .chars()
        .map(|character| {
            if invalid_path_char(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let name = name.trim_end_matches(['.', ' ']);
    let base = if name.is_empty() {
        format!("snippet-{}", index + 1)
    } else {
        split_extension(name).0.into()
    };
    format!("{base}.{extension}")
}

pub(super) fn forced_extension(value: &str) -> io::Result<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let extension = value.trim().trim_start_matches('.');
    if extension.is_empty()
        || extension.len() > 64
        || extension.ends_with(['.', ' '])
        || extension.chars().any(invalid_path_char)
    {
        return Err(invalid(
            "Enter a filename extension up to 64 bytes without path separators",
        ));
    }
    Ok(Some(extension.into()))
}

fn invalid_path_char(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        )
}

fn protect_device_name(name: String) -> String {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if reserved { format!("_{name}") } else { name }
}

fn mode(snippet: &Snippet, file: &str) -> u32 {
    let executable = snippet.metadata().executable.unwrap_or_else(|| {
        let content = snippet.content();
        content
            .strip_prefix('\u{feff}')
            .unwrap_or(content)
            .starts_with("#!")
            || matches!(
                split_extension(file).1.to_ascii_lowercase().as_str(),
                ".sh" | ".bash" | ".zsh" | ".ksh" | ".py" | ".pl" | ".rb" | ".groovy" | ".ps1"
            )
    });
    if executable { 0o755 } else { 0o644 }
}

fn default_extension(language: &str) -> &'static str {
    match language {
        "sh" | "shell" | "zsh" | "bash" => ".sh",
        "py" | "python" | "python3" => ".py",
        "pl" | "perl" => ".pl",
        "rb" | "ruby" => ".rb",
        "lua" | "luajit" => ".lua",
        "js" | "javascript" | "node" | "nodejs" => ".js",
        "ts" | "typescript" => ".ts",
        "groovy" => ".groovy",
        "ps" | "ps1" | "pwsh" | "powershell" => ".ps1",
        "java" => ".java",
        "json" => ".json",
        "yaml" | "yml" => ".yml",
        "xml" => ".xml",
        "markdown" | "md" => ".md",
        "asciidoctor" | "asciidoc" | "adoc" => ".adoc",
        "sql" => ".sql",
        "dockerfile" => ".Dockerfile",
        "properties" | "ini" => ".properties",
        "html" => ".html",
        _ => ".txt",
    }
}
