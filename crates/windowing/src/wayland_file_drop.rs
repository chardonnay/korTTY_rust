//! File paths received from a Wayland desktop drag source.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

use smithay_clipboard::file_drop;

use crate::wayland_drag::Surface;

pub use file_drop::{Permit, Watch};

const MAX_LIST_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PATHS: usize = 256;

#[derive(Clone, Debug)]
pub enum Event {
    Dropped {
        id: u64,
        position: (f64, f64),
        permit: Permit,
    },
    Received {
        id: u64,
        data: std::sync::Arc<[u8]>,
    },
    Cancelled {
        id: u64,
    },
}

pub fn watch(surface: Surface, notify: impl Fn(Event) + Send + Sync + 'static) -> Option<Watch> {
    file_drop::watch(surface.display, surface.surface, move |event| match event {
        file_drop::Event::Dropped { id, target, permit } if target.surface == surface.surface => {
            notify(Event::Dropped {
                id,
                position: (target.x, target.y),
                permit,
            });
        }
        file_drop::Event::Received { id, data } => notify(Event::Received {
            id,
            data: data.into(),
        }),
        file_drop::Event::Cancelled { id } => notify(Event::Cancelled { id }),
        _ => {}
    })
}

/// Decode an entire URI list or reject it if any item is malformed or nonlocal.
/// The caller verifies file types and transfer policy before opening paths.
pub fn local_paths(data: &[u8]) -> Option<Vec<PathBuf>> {
    if data.len() > MAX_LIST_BYTES {
        return None;
    }
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for line in data.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let path = local_path(line)?;
        if seen.insert(path.clone()) {
            if paths.len() == MAX_PATHS {
                return None;
            }
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

fn local_path(uri: &[u8]) -> Option<PathBuf> {
    let scheme = uri.get(..7)?;
    if !scheme.eq_ignore_ascii_case(b"file://") {
        return None;
    }
    let rest = &uri[7..];
    let slash = rest.iter().position(|byte| *byte == b'/')?;
    let host = &rest[..slash];
    if !host.is_empty() && !host.eq_ignore_ascii_case(b"localhost") {
        return None;
    }
    let path = &rest[slash + 1..];
    let path = path.strip_suffix(b"/").unwrap_or(path);
    let mut decoded = Vec::with_capacity(path.len().min(MAX_PATH_BYTES));
    for component in path.split(|byte| *byte == b'/') {
        let start = decoded.len() + 1;
        decoded.push(b'/');
        decode_component(component, &mut decoded)?;
        if matches!(&decoded[start..], b"" | b"." | b"..") {
            return None;
        }
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

fn decode_component(component: &[u8], path: &mut Vec<u8>) -> Option<()> {
    let mut input = component.iter().copied();
    while let Some(byte) = input.next() {
        let byte = if byte == b'%' {
            hex(input.next()?)? * 16 + hex(input.next()?)?
        } else if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&byte) {
            byte
        } else {
            return None;
        };
        if byte.is_ascii_control() || byte == b'/' || path.len() == MAX_PATH_BYTES {
            return None;
        }
        path.push(byte);
    }
    Some(())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt as _;

    use super::*;

    #[test]
    fn reads_local_uris_with_comments_escaping_and_duplicate_paths() {
        let paths = local_paths(
            b"# selection\r\nfile:///tmp/a%20b\r\nfile://LOCALHOST/tmp/a%20b\n\nfile:///tmp/%ff%23/\n",
        )
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/tmp/a b"));
        assert_eq!(paths[1].as_os_str().as_bytes(), b"/tmp/\xff#");
    }

    #[test]
    fn rejects_a_list_containing_an_unsafe_or_malformed_uri() {
        for rejected in [
            "https://localhost/tmp/a",
            "file://remote/tmp/a",
            "file://localhost@remote/tmp/a",
            "file://localhost:80/tmp/a",
            "file:relative",
            "file:///",
            "file:////tmp/a",
            "file:///tmp/../a",
            "file:///tmp/%2E/a",
            "file:///tmp/%2fetc",
            "file:///tmp/%00a",
            "file:///tmp/%0da",
            "file:///tmp/a?query",
            "file:///tmp/a#fragment",
            "file:///tmp/a%zz",
            "file:///tmp/a%",
            "file:///tmp/a b",
        ] {
            let data = format!("file:///tmp/valid\n{rejected}\n");
            assert!(local_paths(data.as_bytes()).is_none(), "{rejected}");
        }
        assert!(local_paths(b"# empty\r\n").is_none());
    }

    #[test]
    fn enforces_size_path_length_and_distinct_path_limits() {
        let max_path = format!("file:///{}", "a".repeat(MAX_PATH_BYTES - 1));
        assert!(local_paths(max_path.as_bytes()).is_some());
        assert!(local_paths(format!("{max_path}a").as_bytes()).is_none());
        assert!(local_paths(&vec![b' '; MAX_LIST_BYTES + 1]).is_none());
        let many = (0..MAX_PATHS)
            .map(|index| format!("file:///tmp/{index}\n"))
            .collect::<String>();
        assert_eq!(local_paths(many.as_bytes()).unwrap().len(), MAX_PATHS);
        assert!(local_paths(format!("{many}file:///extra").as_bytes()).is_none());
    }
}
