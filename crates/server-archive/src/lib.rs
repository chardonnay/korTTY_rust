//! Native archive decoding and a bounded, versioned server extraction protocol.

#[cfg(feature = "decoder")]
pub mod decode;
pub mod draft_replace;
pub mod manifest;
pub mod protocol;
pub mod rsync_tree;

pub const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Zip,
    Tar,
    TarGzip,
    TarBzip2,
    TarXz,
    SevenZip,
}

impl Format {
    /// Returns the codec and original name with the longest recognized suffix removed.
    pub fn detect(name: &str) -> Option<(Self, &str)> {
        const SUFFIXES: &[(&str, Format)] = &[
            (".tar.bz2", Format::TarBzip2),
            (".tar.gz", Format::TarGzip),
            (".tar.xz", Format::TarXz),
            (".tbz2", Format::TarBzip2),
            (".tbz", Format::TarBzip2),
            (".tgz", Format::TarGzip),
            (".txz", Format::TarXz),
            (".tar", Format::Tar),
            (".zip", Format::Zip),
            (".7z", Format::SevenZip),
        ];
        SUFFIXES.iter().find_map(|(suffix, format)| {
            let (base, actual) = name.split_at_checked(name.len().checked_sub(suffix.len())?)?;
            (!base.is_empty() && actual.eq_ignore_ascii_case(suffix)).then_some((*format, base))
        })
    }
}
