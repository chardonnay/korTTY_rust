//! Path validation for an administrator-provisioned copy of a remote policy model.

use std::io;
use std::path::{Component, Path};

use crate::clipboard_policy::{PolicyModel, PolicyModelRuntime};

pub(super) fn path(definition: &PolicyModel) -> io::Result<Option<&Path>> {
    let Some(cache) = definition.protected_cache.as_deref() else {
        return Ok(None);
    };
    if definition.runtime != PolicyModelRuntime::Gguf || definition.digest.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A protected policy model cache requires a GGUF model and a configured SHA-256 digest",
        ));
    }

    let path = Path::new(cache);
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "A protected policy model cache requires an ordinary absolute path to a local .gguf file",
        )
    };
    if !path.is_absolute()
        || cache.len() > 4096
        || cache.chars().any(char::is_control)
        || path.components().any(|part| part == Component::ParentDir)
        || !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
    {
        return Err(invalid());
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;

        let mut components = path.components();
        if !matches!(
            components.next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        ) || components.any(|part| {
            matches!(part, Component::Normal(name) if name.to_str().is_some_and(|name| name.contains(':')))
        }) {
            return Err(invalid());
        }
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const CACHE: &str = "/opt/AI Models/model.GGUF";
    #[cfg(windows)]
    const CACHE: &str = r"C:\Program Files\AI Models\model.GGUF";

    fn definition(cache: Option<&str>) -> PolicyModel {
        PolicyModel {
            id: "policy-test".to_owned(),
            name: "Test model".to_owned(),
            runtime: PolicyModelRuntime::Gguf,
            source: "https://models.invalid/model.gguf".to_owned(),
            digest: Some([0xab; 32]),
            protected_cache: cache.map(str::to_owned),
        }
    }

    #[test]
    fn protected_cache_requires_a_gguf_pin_when_configured() {
        let mut model = definition(None);
        assert_eq!(path(&model).unwrap(), None);

        model.protected_cache = Some(CACHE.to_owned());
        assert_eq!(path(&model).unwrap(), Some(Path::new(CACHE)));

        model.digest = None;
        assert!(path(&model).is_err());

        model.digest = Some([0xab; 32]);
        model.runtime = PolicyModelRuntime::Mlx;
        assert!(path(&model).is_err());
    }

    #[test]
    fn protected_cache_requires_an_ordinary_absolute_local_path() {
        for cache in ["", "model.gguf", "https://models.invalid/model.gguf"] {
            assert!(path(&definition(Some(cache))).is_err(), "{cache:?}");
        }

        #[cfg(unix)]
        let invalid = [
            "/opt/model.bin",
            "/opt/../model.gguf",
            "/opt/line\nbreak.gguf",
        ];
        #[cfg(windows)]
        let invalid = [
            r"C:\models\model.bin",
            r"C:\models\..\model.gguf",
            r"C:\models\file:stream.gguf",
            r"C:models\model.gguf",
            r"\\server\models\model.gguf",
        ];
        for cache in invalid {
            assert!(path(&definition(Some(cache))).is_err(), "{cache:?}");
        }
    }
}
