//! Opt-in local automation transport shared by the desktop and `kortty-cli`.

#![deny(unsafe_code)]

mod client;
mod endpoint;
pub mod mcp;
pub mod policy_value;
mod process;
mod protocol;
mod server;

pub use client::{Client, ClientError};
pub use endpoint::profile_root;
pub use protocol::{
    AGENT_START_WAIT_DEFAULT_MS, AGENT_WAIT_DEFAULT_MS, API, ErrorCode, Fault, Method,
    PROTOCOL_VERSION, Params, ReadMode, Screen, TAB_CREATE_ISOLATIONS, TabCreateParams,
    tab_create_result,
};
pub use server::{Reply, Request, Server};
mod events;
pub use events::Events;
