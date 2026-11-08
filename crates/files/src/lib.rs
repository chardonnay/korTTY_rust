//! Platform file primitives for the kortty desktop.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod darwin_acl;

#[cfg(target_os = "macos")]
pub use darwin_acl::require_no_extended_acl;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use windows::{open_regular_without_reparse, rename_no_replace};

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod tree;
