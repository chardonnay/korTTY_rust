//! Native Wayland Copy drags containing ordinary desktop file URIs.

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, PathBuf};
use std::sync::Arc;

use smithay_clipboard::{drag::DragTarget, file_drag};

use crate::wayland_drag::Surface;

pub use file_drag::{Operation, Outcome};

const MAX_FILES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;
const MAX_URI_BYTES: usize = 1024 * 1024;

pub fn start(
    surface: Surface,
    position: (f64, f64),
    paths: &[PathBuf],
    token: [u8; 16],
    notify: impl FnOnce(Outcome) + Send + 'static,
) -> Option<Operation> {
    use std::fmt::Write as _;

    let payload = file_uris(paths)?;
    let mut marker = String::from("application/x-kortty-file-drag-");
    for byte in token {
        let _ = write!(marker, "{byte:02x}");
    }
    let origin = DragTarget {
        surface: surface.surface,
        x: position.0,
        y: position.1,
    };
    file_drag::start(surface.display, origin, payload, marker, notify)
}

pub(super) fn file_uris(paths: &[PathBuf]) -> Option<Arc<[u8]>> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return None;
    }
    let mut seen = HashSet::new();
    let mut list = Vec::new();
    for path in paths {
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > MAX_PATH_BYTES
            || !path.is_absolute()
            || path.file_name().is_none()
            || path.components().any(|part| part == Component::ParentDir)
            || bytes.iter().any(u8::is_ascii_control)
        {
            return None;
        }
        if !seen.insert(path) {
            continue;
        }
        list.extend_from_slice(b"file://");
        for byte in bytes.iter().copied() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                list.push(byte);
            } else {
                let hex = b"0123456789ABCDEF";
                list.extend_from_slice(&[
                    b'%',
                    hex[usize::from(byte / 16)],
                    hex[usize::from(byte % 16)],
                ]);
            }
        }
        list.extend_from_slice(b"\r\n");
        if list.len() > MAX_URI_BYTES {
            return None;
        }
    }
    Some(list.into())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use super::*;

    #[test]
    fn uri_offer_round_trips_unicode_and_literal_delimiters() {
        let paths = vec![
            PathBuf::from("/tmp/ü # +%"),
            PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec())),
        ];
        let mut selection = paths.clone();
        selection.push(paths[0].clone());
        let encoded = file_uris(&selection).unwrap();
        assert_eq!(crate::wayland_file_drop::local_paths(&encoded), Some(paths));
    }

    #[test]
    fn only_bounded_absolute_local_paths_are_offered() {
        for path in ["/", "relative", "/tmp/../file", "/tmp/control\nname"] {
            assert!(file_uris(&[path.into()]).is_none());
        }
        assert!(file_uris(&vec!["/tmp/file".into(); MAX_FILES + 1]).is_none());
    }
}
