use crate::protocol::WorkerStatus;
use chrono::Utc;
use ploy_operator_contracts::ObservedState;

pub fn heartbeat(status: &mut WorkerStatus) {
    status.last_heartbeat = Utc::now();
    if matches!(status.observed_state, ObservedState::Starting) {
        status.observed_state = ObservedState::Running;
    }
}
