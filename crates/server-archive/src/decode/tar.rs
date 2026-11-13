use std::io::{self, Read};

use crate::manifest::{MAX_EXPANDED_BYTES, MAX_MEMBERS, MAX_PATH_BYTES, MemberKind};

use super::{Output, invalid, unsupported_entry};

const MAX_METADATA_ENTRY: u64 = 32 * 1024;
const MAX_TAR_BYTES: u64 = MAX_EXPANDED_BYTES + 24 * 1024 * 1024;

pub(super) fn visit(reader: impl Read, output: &mut Output<'_>) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader.take(MAX_TAR_BYTES + 1));
    let mut extensions = Extensions::default();
    // Raw mode lets us bound each extension before tar allocates its contents.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        if index >= MAX_MEMBERS * 4 {
            return Err(invalid("The TAR has too many headers"));
        }
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_local_extensions() || kind.is_pax_global_extensions() {
            extensions.read_pax(&mut entry, kind.is_pax_global_extensions())?;
            continue;
        }
        if kind.is_gnu_longname() || kind.is_gnu_longlink() {
            extensions.read_gnu(&mut entry, kind.is_gnu_longlink())?;
            continue;
        }

        if extensions
            .size
            .take()
            .is_some_and(|size| size != entry.size())
        {
            return Err(invalid("PAX size disagrees with the TAR entry header"));
        }
        let path = match extensions.path.take() {
            Some(path) => path,
            None => utf8(&entry.path_bytes())?.to_owned(),
        };
        let link = extensions.link.take();
        if kind.is_symlink() || kind.is_hard_link() {
            if entry.size() != 0 {
                return Err(invalid("TAR link entries must not carry file data"));
            }
            let target = match link {
                Some(target) => target,
                None => utf8(&entry.link_name_bytes().ok_or_else(unsupported_entry)?)?.to_owned(),
            };
            output.link(
                &path,
                if kind.is_symlink() {
                    MemberKind::Symlink { target }
                } else {
                    MemberKind::Hardlink { target }
                },
            )?;
        } else {
            if link.is_some() {
                return Err(invalid("TAR link metadata precedes a non-link member"));
            }
            let kind = if kind.is_dir() {
                MemberKind::Directory
            } else if kind.is_file() {
                MemberKind::File
            } else {
                return Err(unsupported_entry());
            };
            output.content(&path, kind, entry.size(), &mut entry)?;
        }
    }
    if extensions.path.is_some() || extensions.link.is_some() || extensions.size.is_some() {
        return Err(invalid("TAR metadata has no corresponding member"));
    }
    verify_end(archive.into_inner())
}

fn verify_end<R: Read>(mut input: io::Take<R>) -> io::Result<()> {
    // The TAR iterator stops at the first zero header. Consume the second
    // block and any zero padding so compressed streams verify their trailers.
    let mut second = [0; 512];
    input.read_exact(&mut second)?;
    if second.iter().any(|&byte| byte != 0) {
        return Err(invalid("TAR is missing its terminating zero blocks"));
    }
    let mut padding = [0; 8192];
    loop {
        let count = input.read(&mut padding)?;
        if count == 0 {
            break;
        }
        if padding[..count].iter().any(|&byte| byte != 0) {
            return Err(invalid("Unexpected data follows the TAR archive"));
        }
    }
    if input.limit() == 0 {
        return Err(invalid("The expanded TAR stream exceeds its size limit"));
    }
    Ok(())
}

#[derive(Default)]
struct Extensions {
    path: Option<String>,
    link: Option<String>,
    size: Option<u64>,
}

impl Extensions {
    fn read_pax(&mut self, entry: &mut tar::Entry<'_, impl Read>, global: bool) -> io::Result<()> {
        if entry.size() > MAX_METADATA_ENTRY {
            return Err(invalid("PAX metadata exceeds its size limit"));
        }
        let records = entry.pax_extensions()?.ok_or_else(unsupported_entry)?;
        for record in records {
            let record = record?;
            let key = record.key_bytes();
            let value = record.value_bytes();
            match key {
                b"path" | b"linkpath" | b"size" if global => {
                    return Err(invalid("Global PAX path or size overrides are unsupported"));
                }
                b"path" => set_once(&mut self.path, utf8(value)?.to_owned())?,
                b"linkpath" => set_once(&mut self.link, utf8(value)?.to_owned())?,
                b"size" => {
                    let size = utf8(value)?
                        .parse()
                        .map_err(|_| invalid("Invalid PAX member size"))?;
                    set_once(&mut self.size, size)?;
                }
                _ if key.starts_with(b"GNU.sparse") || key == b"SCHILY.realsize" => {
                    return Err(invalid("Sparse TAR entries are unsupported"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn read_gnu(&mut self, entry: &mut tar::Entry<'_, impl Read>, link: bool) -> io::Result<()> {
        if entry.size() == 0 || entry.size() > (MAX_PATH_BYTES + 1) as u64 {
            return Err(invalid("GNU TAR path exceeds the archive path limit"));
        }
        let mut data = vec![0; entry.size() as usize];
        entry.read_exact(&mut data)?;
        let data = data
            .strip_suffix(&[0])
            .ok_or_else(|| invalid("GNU TAR path has no terminator"))?;
        let value = utf8(data)?.to_owned();
        set_once(if link { &mut self.link } else { &mut self.path }, value)
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> io::Result<()> {
    if slot.replace(value).is_some() {
        return Err(invalid("Ambiguous TAR metadata for one member"));
    }
    Ok(())
}

fn utf8(bytes: &[u8]) -> io::Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| invalid("TAR member names and links must use UTF-8"))
}
