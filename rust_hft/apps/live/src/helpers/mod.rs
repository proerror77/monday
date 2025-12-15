pub mod dry_run;
pub mod inference;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod sentinel;
#[cfg(feature = "grpc")]
pub mod grpc;

// Balance sync module available but not re-exported (requires ExecutionClient integration)
#[allow(dead_code)]
pub mod balance_sync;

pub use dry_run::run_dry_run_if_enabled;
pub use inference::spawn_inference_worker;
#[cfg(feature = "metrics")]
pub use metrics::spawn_metrics_server;
pub use sentinel::{spawn_sentinel_worker, SentinelWorkerConfig};
#[cfg(feature = "grpc")]
pub use grpc::spawn_grpc_server;
