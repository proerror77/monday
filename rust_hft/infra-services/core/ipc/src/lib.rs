//! HFT Control Plane IPC
//!
//! Provides Unix Domain Socket based IPC for control plane operations.
//! Messages are serialized using MessagePack (rmp-serde) for efficiency.
//!
//! # Feature Flags
//!
//! - `ipc` - Enables full IPC functionality (server, client, MessagePack serialization)
//!
//! Without the `ipc` feature, only message type definitions are available for use
//! in configuration and type-level programming.

// Message definitions are always available
pub mod messages;
pub use messages::*;

// IPC functionality requires the 'ipc' feature
#[cfg(feature = "ipc")]
pub mod client;
#[cfg(feature = "ipc")]
pub mod handlers;
#[cfg(feature = "ipc")]
pub mod server;

#[cfg(feature = "ipc")]
pub use client::IPCClient;
#[cfg(feature = "ipc")]
pub use server::IPCServer;

/// IPC communication errors
#[derive(thiserror::Error, Debug)]
pub enum IPCError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[cfg(feature = "ipc")]
    #[error("Serialization error: {0}")]
    Serialization(#[from] rmp_serde::encode::Error),

    #[cfg(feature = "ipc")]
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
