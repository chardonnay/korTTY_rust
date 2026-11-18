//! Bounded JSON storage for recurring approvals. Callers manage write locks.

use std::io::{self, Read as _};

use serde::de::DeserializeOwned;

use crate::private_files::Directory;

const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub(super) fn read<T: Default + DeserializeOwned>(
    directory: &Directory,
    name: &str,
    valid: fn(&T) -> bool,
    invalid: fn() -> io::Error,
) -> io::Result<T> {
    let file = match directory.open_private_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    check_size(&bytes, invalid)?;
    let index = serde_json::from_slice(&bytes)?;
    valid(&index).then_some(index).ok_or_else(invalid)
}

pub(super) fn write(
    directory: &Directory,
    name: &str,
    bytes: &[u8],
    invalid: fn() -> io::Error,
) -> io::Result<()> {
    check_size(bytes, invalid)?;
    directory.replace_file(name, bytes)?;
    directory.sync()
}

fn check_size(bytes: &[u8], invalid: fn() -> io::Error) -> io::Result<()> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    Ok(())
}
