pub mod accounts;
pub mod audit;
pub mod control_plane;
pub mod deployments;
pub mod health;
pub mod system;

pub use accounts::AccountSnapshot;
pub use audit::{AuditEvent, AuditLog};
pub use control_plane::ControlPlane;
pub use deployments::{DeploymentRecord, DeploymentRegistry};
pub use health::{snapshot as health_snapshot, HealthSnapshot};
pub use system::SystemService;

pub const CRATE_MARKER: &str = "ploy-platform";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
