//! The destination permission fields retained by Java's folder-sync finalizer.

use std::fs::File;
use std::io;

#[cfg(unix)]
pub(in crate::sftp_window::worker) mod unix;

#[cfg(windows)]
use kortty_files::ReplacementMetadata as NativeMetadata;
#[cfg(unix)]
use unix::Metadata as NativeMetadata;

pub(super) struct Metadata(NativeMetadata);

impl Metadata {
    pub(super) fn capture(file: &File) -> io::Result<Self> {
        NativeMetadata::capture(file).map(Self)
    }

    pub(super) fn require_matches(&self, file: &File) -> io::Result<()> {
        #[cfg(unix)]
        return self.0.verify_original(file);
        #[cfg(windows)]
        self.0.require_matches(file)
    }

    pub(super) fn apply_to(&self, file: &File) -> io::Result<()> {
        self.0.apply_to(file)
    }

    #[cfg(windows)]
    pub(super) fn review_bytes(&self) -> Vec<u8> {
        self.0.review_bytes()
    }
}
