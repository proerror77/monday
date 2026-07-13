use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub overall_status: String,
    pub active_deployments: usize,
}

pub fn snapshot(active_deployments: usize) -> HealthSnapshot {
    HealthSnapshot {
        overall_status: if active_deployments == 0 {
            "idle".to_string()
        } else {
            "running".to_string()
        },
        active_deployments,
    }
}
