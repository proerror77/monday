//! Research data/control/execution boundaries. No trading or cloud provisioning authority.
#[cfg(feature = "control")]
pub mod agent_api;
pub mod build;
#[cfg(feature = "control")]
pub mod clickhouse;
pub mod coding_agent;
pub mod data;
pub mod execution;
pub mod orchestrator;
#[cfg(feature = "control")]
pub mod postgres;
pub mod prepared;
pub mod research;
#[cfg(feature = "control")]
pub mod service;

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
