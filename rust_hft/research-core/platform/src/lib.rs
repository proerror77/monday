//! Research data/control/execution boundaries. No trading or cloud provisioning authority.
#[cfg(feature = "control")]
pub mod agent_api;
#[cfg(feature = "gateway")]
pub mod artifact_gateway;
#[cfg(feature = "control")]
pub mod block_objects;
pub mod build;
#[cfg(feature = "control")]
pub mod clickhouse;
pub mod coding_agent;
pub mod execution;
pub mod orchestrator;
#[cfg(feature = "control")]
pub mod postgres;
pub mod preparation;
#[cfg(feature = "control")]
pub mod release;
pub mod research;
#[cfg(feature = "control")]
pub mod service;
#[cfg(feature = "control")]
pub mod session;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn identity(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(sha256(&serde_json::to_vec(value)?))
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[cfg(feature = "control")]
pub mod worker;

#[cfg(feature = "control")]
pub mod transport;
