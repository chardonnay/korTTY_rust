//! Linux POSIX ACL xattrs through held descriptors, without a libacl dependency.

use std::fs::File;
use std::io;

use rustix::fs::{XattrFlags, fgetxattr, fremovexattr, fsetxattr};
use rustix::io::Errno;

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";
const MAX_ACL_BYTES: usize = 64 * 1024;

#[derive(Eq, PartialEq)]
pub(super) struct Acl(Option<Vec<u8>>);

impl Acl {
    pub(super) fn capture(file: &File) -> io::Result<Self> {
        Self::read(file, ACCESS)
    }

    pub(super) fn capture_default(file: &File) -> io::Result<Self> {
        Self::read(file, DEFAULT)
    }

    fn read(file: &File, name: &str) -> io::Result<Self> {
        let mut bytes = vec![0; MAX_ACL_BYTES];
        match fgetxattr(file, name, bytes.as_mut_slice()) {
            Ok(size) => {
                bytes.truncate(size);
                Ok(Self(Some(bytes)))
            }
            Err(Errno::NODATA | Errno::OPNOTSUPP) => Ok(Self(None)),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn apply_to(&self, file: &File) -> io::Result<()> {
        self.apply_named(file, ACCESS)
    }

    pub(super) fn apply_default_to(&self, file: &File) -> io::Result<()> {
        self.apply_named(file, DEFAULT)
    }

    fn apply_named(&self, file: &File, name: &str) -> io::Result<()> {
        if let Some(bytes) = &self.0 {
            fsetxattr(file, name, bytes, XattrFlags::empty())?;
        } else if let Err(error) = fremovexattr(file, name)
            && !matches!(error, Errno::NODATA | Errno::OPNOTSUPP)
        {
            return Err(error.into());
        }
        Ok(())
    }
}
