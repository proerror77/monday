//! Immutable research worker wire identities and strict receiver validation.
//! No database, scheduler, provider client, signature issuance or training engine.
pub mod admission;
pub mod campaign_result;
pub mod execution;
pub mod research;
pub mod task;

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
