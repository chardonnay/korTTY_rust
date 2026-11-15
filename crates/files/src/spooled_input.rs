use std::io::{self, Read};
use std::path::Path;

/// Copies an input into an anonymous file in the caller's private temporary directory,
/// then borrows a read-only view.
/// Returns `None` when the input exceeds the requested size.
pub fn with_spooled_input<T>(
    source: impl Read,
    directory: &Path,
    limit: usize,
    consume: impl FnOnce(&[u8]) -> T,
) -> io::Result<Option<T>> {
    let mut file = tempfile::tempfile_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata()?.nlink() != 0 {
            return Err(io::Error::other(
                "Could not unlink the temporary input file",
            ));
        }
    }
    let count = io::copy(
        &mut source.take((limit as u64).saturating_add(1)),
        &mut file,
    )?;
    if count > limit as u64 {
        return Ok(None);
    }
    if count == 0 {
        return Ok(Some(consume(&[])));
    }
    // SAFETY: The file is anonymous on Unix and opened without sharing on Windows.
    // Its only handle stays in this function, which stops writing before mapping it.
    let bytes = unsafe { memmap2::MmapOptions::new().map(&file)? };
    Ok(Some(consume(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spooled_input_maps_only_the_bounded_bytes_and_releases_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = &b"bounded input"[..];
        for (limit, expected) in [(source.len(), Some(source.to_vec())), (1, None)] {
            assert_eq!(
                with_spooled_input(source, directory.path(), limit, <[u8]>::to_vec).unwrap(),
                expected
            );
            assert_eq!(directory.path().read_dir().unwrap().count(), 0);
        }
        assert_eq!(
            with_spooled_input(&[][..], directory.path(), 0, <[u8]>::len).unwrap(),
            Some(0)
        );
    }
}
