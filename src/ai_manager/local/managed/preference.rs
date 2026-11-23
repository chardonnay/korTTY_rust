//! The manager's default model marker is independent of AI request settings.

use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::local_ai::managed_models::valid_id;
use crate::private_files::{Directory, FileStamp, require_single_link};

use super::ModelId;

const FILE_NAME: &str = "managed-model-default.json";
const MUTATION_LOCK: &str = ".managed-model-default.lock";
const GGUF_VERSION: u8 = 1;
const TYPED_VERSION: u8 = 2;
const MAX_BYTES: u64 = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SavedDefault {
    Missing,
    Stored(Option<ModelId>),
}

impl SavedDefault {
    pub(super) fn resolve(&self, legacy: Option<&str>) -> Option<ModelId> {
        match self {
            Self::Missing => legacy.map(|id| ModelId::Gguf(id.to_owned())),
            Self::Stored(id) => id.clone(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelFormat {
    Mlx,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_format: Option<ModelFormat>,
}

pub(in crate::ai_manager) struct Change {
    pub(super) previous: SavedDefault,
    pub(super) model_id: Option<ModelId>,
}

impl Change {
    pub(in crate::ai_manager) fn commit(self, root: &Path) -> io::Result<Option<ModelId>> {
        if self
            .model_id
            .as_ref()
            .is_some_and(|id| !valid_id(id.as_str()))
        {
            return Err(invalid("Invalid managed model ID"));
        }
        let directory = Directory::open(root, true)?;
        let lock = directory.open_lock(MUTATION_LOCK)?;
        require_single_link(&lock)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another window is saving the default model. Refresh and try again.",
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        let (current, stamp) = read(&directory)?;
        if current != self.previous {
            return Err(io::Error::other(
                "The default model changed. Refresh managed models and try again.",
            ));
        }

        let model_format = match &self.model_id {
            Some(ModelId::Mlx(_)) => Some(ModelFormat::Mlx),
            _ => None,
        };
        let record = Record {
            version: if model_format.is_some() {
                TYPED_VERSION
            } else {
                GGUF_VERSION
            },
            model_id: self.model_id.as_ref().map(|id| id.as_str().to_owned()),
            model_format,
        };
        let bytes = serde_json::to_vec(&record).map_err(invalid)?;
        if let Some(stamp) = stamp {
            directory.replace_with(
                FILE_NAME,
                |file| file.write_all(&bytes),
                || {
                    let current = directory.open_private_file(FILE_NAME)?;
                    require_single_link(&current)?;
                    stamp.require_unchanged(&current)
                },
            )?;
        } else {
            directory.publish_copy(FILE_NAME, &bytes)?;
        }
        Ok(self.model_id)
    }
}

pub(super) fn load(root: &Path) -> io::Result<SavedDefault> {
    match Directory::open(root, false) {
        Ok(directory) => read(&directory).map(|(default, _)| default),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SavedDefault::Missing),
        Err(error) => Err(error),
    }
}

fn read(directory: &Directory) -> io::Result<(SavedDefault, Option<FileStamp>)> {
    let file = match directory.open_private_file(FILE_NAME) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((SavedDefault::Missing, None));
        }
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("The managed model default exceeds the size limit"));
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(invalid)?;
    if record.model_id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err(invalid("Invalid managed model default"));
    }
    let model = match (record.version, record.model_format, record.model_id) {
        (GGUF_VERSION, None, model) => model.map(ModelId::Gguf),
        (TYPED_VERSION, Some(ModelFormat::Mlx), Some(id)) => Some(ModelId::Mlx(id)),
        _ => return Err(invalid("Invalid managed model default")),
    };
    Ok((SavedDefault::Stored(model), Some(stamp)))
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_model_default_compare_and_swap_preserves_explicit_clear() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let model = "0123456789abcdef0123456789abcdef";
        let gguf = ModelId::Gguf(model.into());
        let missing = load(&root)?;
        assert_eq!(missing.resolve(Some(model)), Some(gguf.clone()));
        let set = Change {
            previous: missing.clone(),
            model_id: Some(gguf.clone()),
        };
        assert_eq!(set.commit(&root)?, Some(gguf.clone()));
        let saved = load(&root)?;
        assert_eq!(saved.resolve(None), Some(gguf));
        let stale = Change {
            previous: missing,
            model_id: None,
        };
        assert!(stale.commit(&root).is_err());
        assert_eq!(load(&root)?, saved);

        let clear = Change {
            previous: saved,
            model_id: None,
        };
        assert!(clear.commit(&root)?.is_none());
        assert_eq!(load(&root)?.resolve(Some(model)), None);
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(FILE_NAME))?)?;
        assert_eq!(record, serde_json::json!({"version": 1, "model_id": null}));
        Ok(())
    }

    #[test]
    fn typed_mlx_default_keeps_the_same_gguf_id_distinct() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let id = "0123456789abcdef0123456789abcdef";
        let root_dir = Directory::open(&root, false)?;
        root_dir.publish_copy(
            FILE_NAME,
            format!(r#"{{"version":1,"model_id":"{id}"}}"#).as_bytes(),
        )?;
        let previous = load(&root)?;
        assert_eq!(previous.resolve(None), Some(ModelId::Gguf(id.into())));
        let mlx = ModelId::Mlx(id.into());
        let mark = Change {
            previous: previous.clone(),
            model_id: Some(mlx.clone()),
        };
        assert_eq!(mark.commit(&root)?, Some(mlx.clone()));
        assert_eq!(load(&root)?.resolve(None), Some(mlx));
        assert!(
            Change {
                previous,
                model_id: None
            }
            .commit(&root)
            .is_err()
        );
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join(FILE_NAME))?)?;
        assert_eq!(
            record,
            serde_json::json!({"version": 2, "model_id": id, "model_format": "mlx"})
        );
        Ok(())
    }
}
