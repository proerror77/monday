//! HFT Ops Agent - LLM-powered autonomous trading system operations
//!
//! This crate provides an autonomous operations agent powered by Claude that can:
//! - Monitor system health and performance
//! - Detect and respond to anomalies
//! - Manage risk parameters dynamically
//! - Deploy new models
//! - Send alerts and log decisions

pub mod agent;
pub mod config;
pub mod grpc_client;
pub mod tools;

pub use agent::HftOpsAgent;
pub use config::AgentConfig;
