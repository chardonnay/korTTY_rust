//! Last normal bounds for native tool and configuration windows.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{MAX_PLACEMENT_BYTES, WindowPlacement, invalid, open_preference, root, save_atomic};

const MAX_KIND_BYTES: usize = 64;

pub(crate) fn load(kind: &'static str) -> io::Result<WindowPlacement> {
    load_from(&root()?, kind)
}

pub(crate) fn save(kind: &'static str, placement: WindowPlacement) -> io::Result<()> {
    save_to(&root()?, kind, placement)
}

fn path(root: &Path, kind: &'static str) -> io::Result<PathBuf> {
    if kind.is_empty()
        || kind.len() > MAX_KIND_BYTES
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid dialog geometry kind",
        ));
    }
    Ok(root.join(format!("dialog-geometry-{kind}.json")))
}

fn load_from(root: &Path, kind: &'static str) -> io::Result<WindowPlacement> {
    let file = open_preference(&path(root, kind)?)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("dialog geometry requires a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLACEMENT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PLACEMENT_BYTES {
        return Err(invalid("dialog geometry exceeds the size limit"));
    }
    let placement: WindowPlacement = serde_json::from_slice(&bytes).map_err(invalid)?;
    validate(placement)?;
    Ok(placement)
}

fn save_to(root: &Path, kind: &'static str, placement: WindowPlacement) -> io::Result<()> {
    let path = path(root, kind)?;
    validate(placement)?;
    save_atomic(&path, &serde_json::to_vec(&placement).map_err(invalid)?)
}

fn validate(placement: WindowPlacement) -> io::Result<()> {
    if placement.valid() {
        Ok(())
    } else {
        Err(invalid("invalid dialog geometry"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::profile::SavedPosition;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let suffix = getrandom::u64().map_err(io::Error::other)?;
            let path = std::env::temp_dir().join(format!("kortty-dialog-geometry-{suffix:016x}"));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const SIZE_ONLY: WindowPlacement = WindowPlacement {
        width: 760.0,
        height: 580.0,
        position: None,
    };

    #[test]
    fn dialog_geometry_keeps_different_kinds_and_preserves_good_bounds() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let root = &scratch.0;
        assert_eq!(
            load_from(root, "settings").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        save_to(root, "settings", SIZE_ONLY)?;
        let position = SavedPosition {
            x: -604.5,
            y: 72.0,
            scale_factor: Some(1.5),
        };
        let other = WindowPlacement {
            width: 950.5,
            height: 690.0,
            position: Some(position),
        };
        save_to(root, "ai-manager", other)?;
        assert_eq!(load_from(root, "settings")?, SIZE_ONLY);
        assert_eq!(load_from(root, "ai-manager")?, other);
        for invalid in [
            WindowPlacement {
                width: f32::NAN,
                ..other
            },
            WindowPlacement {
                position: Some(SavedPosition {
                    scale_factor: Some(0.0),
                    ..position
                }),
                ..other
            },
        ] {
            assert_eq!(
                save_to(root, "ai-manager", invalid).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert_eq!(load_from(root, "ai-manager")?, other);
        Ok(())
    }

    #[test]
    fn dialog_geometry_rejects_invalid_kinds_and_contents() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let root = &scratch.0;
        for kind in [
            "",
            "Settings",
            "../settings",
            "shell/extra",
            "settings_2",
            "ä",
            "0123456789012345678901234567890123456789012345678901234567890123456",
        ] {
            assert_eq!(
                save_to(root, kind, SIZE_ONLY).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(fs::read_dir(root)?.count(), 0);

        let file = path(root, "settings-2")?;
        for malformed in [
            b"not JSON".as_slice(),
            br#"{"width":900,"height":0}"#,
            br#"{"width":900,"height":700,"position":{"x":100001,"y":0}}"#,
            &[b' '; MAX_PLACEMENT_BYTES as usize + 1],
        ] {
            fs::write(&file, malformed)?;
            assert_eq!(
                load_from(root, "settings-2").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dialog_geometry_does_not_follow_a_preference_symlink() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let root = &scratch.0;
        save_to(root, "target", SIZE_ONLY)?;
        let target = path(root, "target")?;
        std::os::unix::fs::symlink(&target, path(root, "settings")?)?;
        assert!(load_from(root, "settings").is_err());

        let replacement = WindowPlacement {
            width: 950.0,
            ..SIZE_ONLY
        };
        save_to(root, "settings", replacement)?;
        assert_eq!(load_from(root, "settings")?, replacement);
        assert_eq!(load_from(root, "target")?, SIZE_ONLY);
        Ok(())
    }
}
