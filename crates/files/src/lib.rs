//! Platform file primitives for the kortty desktop.

#![deny(unsafe_code)]

#[cfg(unix)]
#[allow(unsafe_code)]
pub mod unix_accounts;

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod linux_verity;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod darwin_acl;

#[cfg(target_os = "macos")]
pub use darwin_acl::{
    ExtendedAcl, clear_extended_acl, require_no_extended_acl, require_no_granting_extended_acl,
};

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use windows::{
    available_space, is_local_reference_file, open_regular_without_reparse, rename_no_replace,
};

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod tree;

#[cfg(windows)]
#[allow(unsafe_code)]
mod replacement_metadata;

#[cfg(windows)]
pub use replacement_metadata::ReplacementMetadata;

#[cfg(windows)]
pub use tree::restore_private_file_access;

#[cfg(windows)]
#[allow(unsafe_code)]
mod new_file_access;

#[cfg(windows)]
pub use new_file_access::NewFileAccess;
