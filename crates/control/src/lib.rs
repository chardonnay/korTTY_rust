//! Opt-in local automation transport shared by the desktop and `kortty-cli`.

#![deny(unsafe_code)]

mod client;
mod endpoint;
mod process;
mod protocol;
mod server;

pub use client::{Client, ClientError};
pub use endpoint::profile_root;
pub use protocol::{API, ErrorCode, Fault, Method, PROTOCOL_VERSION, Params, ReadMode, Screen};
pub use server::{Reply, Request, Server};
