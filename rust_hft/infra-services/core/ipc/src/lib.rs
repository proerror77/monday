//! HFT Control Plane IPC
//!
//! Provides Unix Domain Socket based IPC for control plane operations.
//! Messages are serialized using MessagePack (rmp-serde) for efficiency.

use hft_core::Symbol;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub mod client;
pub mod handlers;
pub mod messages;
pub mod server;

pub use client::IPCClient;
pub use messages::*;
pub use server::IPCServer;

/// IPC communication errors
#[derive(thiserror::Error, Debug)]
pub enum IPCError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] rmp_serde::encode::Error),

    #[error("Deserialization error: {0}")]
    Deserialization(#[from] rmp_serde::decode::Error),

    #[error("Command handler error: {0}")]
    Handler(String),

    #[error("Timeout waiting for response")]
    Timeout,
}

pub type IPCResult<T> = Result<T, IPCError>;

/// Default socket path for IPC server
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/hft_control.sock";

/// Maximum message size (1MB)
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
