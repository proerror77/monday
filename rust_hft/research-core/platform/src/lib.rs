//! Research data/control/execution boundaries. No trading or cloud provisioning authority.
#[cfg(feature = "native-admission")]
pub mod admission;
#[cfg(feature = "control")]
pub mod agent_api;
#[cfg(feature = "gateway")]
pub mod artifact_gateway;
#[cfg(feature = "control")]
pub mod artifact_identity;
#[cfg(feature = "artifact-io")]
pub mod artifact_io;
#[cfg(feature = "control")]
pub mod block_objects;
pub mod build;
#[cfg(feature = "native-admission")]
pub mod campaign;
pub mod campaign_result;
#[cfg(feature = "control")]
pub mod clickhouse;
pub mod coding_agent;
pub mod execution;
pub mod orchestrator;
#[cfg(feature = "control")]
pub mod postgres;
pub mod preparation;
#[cfg(any(feature = "control", feature = "release-verification"))]
pub mod release;
#[cfg(feature = "publisher")]
pub mod release_publisher;
pub mod research;
#[cfg(feature = "native-admission")]
pub mod revocation;
#[cfg(feature = "control")]
pub mod service;
#[cfg(feature = "control")]
pub mod session;
#[cfg(feature = "native-admission")]
pub mod terminal_audit;
#[cfg(feature = "artifact-io")]
pub mod worker_configuration;

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

#[cfg(feature = "artifact-io")]
pub mod transport;

#[cfg(feature = "control")]
pub mod foundation;
