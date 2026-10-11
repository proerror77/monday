//! Attempt-scoped artifact IO and bounded private configuration staging.
//! Callers supply admitted context and credentials; this crate cannot issue them.
pub mod archive;
pub mod artifact;
pub mod configuration;
pub mod transport;

use hft_research_worker_contracts::{sha256, valid_digest};
