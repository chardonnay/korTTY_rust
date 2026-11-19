//! Import a schema and its local companion resources as a portable snapshot.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use percent_encoding::percent_decode_str;
use referencing::{Draft, Retrieve, Uri};
use serde_json::Value;
use url::Url;

use super::{BASE_URI, MAX_RESOURCES, MAX_SCHEMA_BYTES, Schema};

pub(crate) fn read(path: &Path, include_related: bool) -> io::Result<Arc<Schema>> {
    let schema = read_file(path)?;
    if !include_related {
        return Ok(Arc::new(schema));
    }
    let canonical = path.canonicalize()?;
    let directory = canonical
        .parent()
        .ok_or_else(|| io::Error::other("The schema has no parent folder."))?
        .to_path_buf();
    let effective = Url::parse(BASE_URI).expect("valid internal schema URI");
    let draft = Draft::default().detect(&schema.root);
    let effective = match draft.create_resource_ref(&schema.root).id() {
        Some(id) => effective.join(id).map_err(io::Error::other)?,
        None => effective,
    };
    let scope = effective.join("./").map_err(io::Error::other)?;
    if !matches!(scope.scheme(), "http" | "https")
        || !scope.username().is_empty()
        || scope.password().is_some()
    {
        return Err(io::Error::other(
            "Local schema references require a relative, HTTP, or HTTPS schema identifier.",
        ));
    }
    let state = Arc::new(Mutex::new(Collected {
        bytes: serde_json::to_vec(&schema.root)?.len() as u64,
        documents: BTreeMap::new(),
    }));
    let retriever = Related {
        directory,
        scope,
        state: state.clone(),
    };
    schema
        .registry_builder()
        .map_err(io::Error::other)?
        .retriever(retriever)
        .prepare()
        .map_err(io::Error::other)?;
    let mut collected = state
        .lock()
        .map_err(|_| io::Error::other("The schema loader stopped."))?;
    Schema::from_documents(schema.root, std::mem::take(&mut collected.documents)).map(Arc::new)
}

fn read_file(path: &Path) -> io::Result<Schema> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_SCHEMA_BYTES {
        return Err(io::Error::other(
            "Choose a regular JSON schema file up to 512 KiB.",
        ));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "The schema path no longer identifies a regular file.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCHEMA_BYTES + 1).read_to_end(&mut bytes)?;
    Schema::parse(&bytes)
}

struct Collected {
    bytes: u64,
    documents: BTreeMap<String, Value>,
}

struct Related {
    directory: PathBuf,
    scope: Url,
    state: Arc<Mutex<Collected>>,
}

impl Retrieve for Related {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let blocked = || io::Error::other("The reference is outside the selected schema folder.");
        let url = Url::parse(uri.as_str())?;
        if url.origin() != self.scope.origin()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || uri.as_str().len() > 4096
        {
            return Err(blocked().into());
        }
        let relative = url
            .path()
            .strip_prefix(self.scope.path())
            .ok_or_else(blocked)?;
        let mut path = self.directory.clone();
        for segment in relative.split('/') {
            let segment = percent_decode_str(segment).decode_utf8()?;
            if segment.is_empty()
                || matches!(segment.as_ref(), "." | "..")
                || segment.contains(['/', '\\', ':'])
                || segment.chars().any(char::is_control)
            {
                return Err(blocked().into());
            }
            path.push(segment.as_ref());
        }
        let path = path.canonicalize()?;
        if !path.starts_with(&self.directory) {
            return Err(blocked().into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("The schema loader stopped."))?;
        if let Some(existing) = state.documents.get(uri.as_str()) {
            return Ok(existing.clone());
        }
        if state.documents.len() >= MAX_RESOURCES || state.bytes >= MAX_SCHEMA_BYTES {
            return Err(
                io::Error::other("Local schema references exceed the snapshot limits.").into(),
            );
        }
        let value = read_file(&path)?.root;
        state.bytes += serde_json::to_vec(&value)?.len() as u64;
        if state.bytes > MAX_SCHEMA_BYTES {
            return Err(io::Error::other("Local schema references exceed 512 KiB.").into());
        }
        state.documents.insert(uri.as_str().into(), value.clone());
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_reference_loading_refuses_other_folders_and_origins() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("schemas");
        fs::create_dir(&folder).unwrap();
        fs::write(temp.path().join("outside.json"), "true").unwrap();
        fs::write(folder.join("outside.json"), "true").unwrap();
        let entry = folder.join("entry.json");
        for reference in [
            "../outside.json",
            "https://other.example.invalid/catalog/outside.json",
            "file:///outside.json",
        ] {
            let root = serde_json::json!({"$id":"https://schemas.example.invalid/catalog/entry.json", "$ref":reference});
            fs::write(&entry, root.to_string()).unwrap();
            assert!(read(&entry, false).is_ok());
            assert!(read(&entry, true).is_err(), "accepted {reference}");
        }
    }
}
