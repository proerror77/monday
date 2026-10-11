//! Research data/control/execution boundaries. No trading or cloud provisioning authority.
#[cfg(feature = "native-admission")]
pub mod admission;
#[cfg(feature = "control")]
pub mod agent_api;
#[cfg(feature = "gateway")]
pub mod artifact_gateway;
#[cfg(feature = "control")]
pub mod artifact_identity;
#[cfg(feature = "control")]
pub mod block_objects;
pub mod build;
#[cfg(feature = "control")]
pub mod build_objects;
#[cfg(feature = "native-admission")]
pub mod campaign;
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
pub mod release_budget;
#[cfg(feature = "publisher")]
pub mod release_oss;
#[cfg(feature = "publisher")]
pub mod release_publisher;
pub mod research;
#[cfg(feature = "researcher-verification")]
pub mod researcher;
#[cfg(feature = "control")]
pub mod retirement;
#[cfg(feature = "native-admission")]
pub mod revocation;
#[cfg(feature = "control")]
pub mod service;
#[cfg(feature = "control")]
pub mod session;
#[cfg(feature = "native-admission")]
pub mod terminal_audit;

use hft_research_worker_contracts::{identity, sha256, valid_digest};

#[cfg(feature = "control")]
pub mod worker;

#[cfg(feature = "control")]
pub mod foundation;
