pub mod health;
pub mod protocol;
pub mod runtime;
pub mod supervisor;

pub use protocol::{WorkerLaunchSpec, WorkerStatus, CANONICAL_CONTROL_GENERATION};
pub use runtime::DeploymentRuntime;
pub use supervisor::WorkerSupervisor;

pub const CRATE_MARKER: &str = "ploy-deployments";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
