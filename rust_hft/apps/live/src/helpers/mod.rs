#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod sentinel;

// Balance sync module available but not re-exported (requires ExecutionClient integration)
#[allow(dead_code)]
pub mod balance_sync;

#[cfg(feature = "grpc")]
pub use grpc::spawn_grpc_server;
#[cfg(feature = "metrics")]
pub use metrics::spawn_metrics_server;
pub use sentinel::{spawn_sentinel_worker, SentinelWorkerConfig};
