//! Immutable CEX time-series inputs shared by control and CPU workers.
//! No database, provider, agent, acquisition or execution dependency.
//! Prediction-market event and settlement inputs remain in their own contracts.
pub mod data;
pub mod prepared;

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
