use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::{
    DeploymentRuntimeMode, DeploymentState, DesiredState, ObservedState,
};
use ploy_platform::ControlPlane;
use std::io;

use crate::live_reconcile_backoff_ms;

#[derive(Debug, Clone)]
pub struct LiveHealthConfig {
    pub listen_addr: String,
    pub live_reconcile_stale_after_ms: u64,
    pub venue_stale_after_ms: u64,
    pub live_reconcile_backoff_base_ms: u64,
    pub live_reconcile_backoff_max_ms: u64,
}

pub fn next_live_reconcile_at(config: &LiveHealthConfig, failures: u32) -> DateTime<Utc> {
    let backoff_ms = live_reconcile_backoff_ms(
        failures,
        config.live_reconcile_backoff_base_ms,
        config.live_reconcile_backoff_max_ms,
    );
    Utc::now() + Duration::milliseconds(backoff_ms as i64)
}

pub fn mark_runtime_healthy(
    control_plane: &mut ControlPlane,
    config: &LiveHealthConfig,
    latest_trade_time: Option<DateTime<Utc>>,
) {
    control_plane.system.note_live_reconcile_healthy();
    control_plane.system.note_source_heartbeat(
        "live_reconcile",
        "live_reconcile",
        Duration::milliseconds(config.live_reconcile_stale_after_ms as i64),
    );
    control_plane.system.note_trade(latest_trade_time);

    if control_plane.system.is_degraded() {
        control_plane.system.mark_recovering(&config.listen_addr);
    } else if control_plane
        .system
        .status()
        .status
        .starts_with("recovering")
    {
        control_plane.system.mark_running(&config.listen_addr);
    } else {
        control_plane.system.mark_running(&config.listen_addr);
    }
}

pub fn mark_venue_healthy(control_plane: &mut ControlPlane, config: &LiveHealthConfig) {
    control_plane.system.note_source_heartbeat(
        "venue:polymarket",
        "venue",
        Duration::milliseconds(config.venue_stale_after_ms as i64),
    );
}

pub fn mark_live_runtime_degraded(
    control_plane: &mut ControlPlane,
    config: &LiveHealthConfig,
    failures: &mut u32,
    next_attempt_at: &mut Option<DateTime<Utc>>,
    last_error: &mut Option<String>,
    err: &io::Error,
) {
    control_plane.system.mark_degraded(&config.listen_addr);
    control_plane.system.note_source_failure(
        "live_reconcile",
        "live_reconcile",
        Duration::milliseconds(config.live_reconcile_stale_after_ms as i64),
        err.to_string(),
    );
    control_plane.system.note_source_failure(
        "venue:polymarket",
        "venue",
        Duration::milliseconds(config.venue_stale_after_ms as i64),
        err.to_string(),
    );

    let current_failures = failures.saturating_add(1);
    *failures = current_failures;
    let next = next_live_reconcile_at(config, current_failures);
    *next_attempt_at = Some(next);
    *last_error = Some(err.to_string());
    control_plane
        .system
        .note_live_reconcile_failure(current_failures, next, err.to_string());

    for record in control_plane.deployments.records() {
        if record.runtime_mode != DeploymentRuntimeMode::Live
            || record.deployment_state == DeploymentState::Archived
            || record.desired_state != DesiredState::Running
        {
            continue;
        }

        control_plane
            .deployments
            .set_observed_state(&record.deployment_id, ObservedState::Degraded);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        mark_live_runtime_degraded, mark_runtime_healthy, mark_venue_healthy,
        next_live_reconcile_at, LiveHealthConfig,
    };
    use chrono::Utc;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::{ControlPlane, DeploymentRecord};
    use rust_decimal_macros::dec;
    use std::io;

    fn config() -> LiveHealthConfig {
        LiveHealthConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            live_reconcile_stale_after_ms: 15_000,
            venue_stale_after_ms: 15_000,
            live_reconcile_backoff_base_ms: 1_000,
            live_reconcile_backoff_max_ms: 30_000,
        }
    }

    #[test]
    fn healthy_and_degraded_transitions_round_trip() {
        let mut control_plane = ControlPlane::default();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Degraded,
        });

        mark_venue_healthy(&mut control_plane, &config());
        mark_runtime_healthy(&mut control_plane, &config(), None);
        assert_eq!(
            control_plane.system.status().status,
            "running@127.0.0.1:8081"
        );

        let mut failures = 0;
        let mut next_attempt_at = None;
        let mut last_error = None;
        mark_live_runtime_degraded(
            &mut control_plane,
            &config(),
            &mut failures,
            &mut next_attempt_at,
            &mut last_error,
            &io::Error::new(io::ErrorKind::Other, "gateway offline"),
        );
        assert_eq!(failures, 1);
        assert!(next_attempt_at.is_some());
        assert_eq!(last_error.as_deref(), Some("gateway offline"));
        assert_eq!(control_plane.system.status().live_reconcile_failures, 1);
    }

    #[test]
    fn local_runtime_health_does_not_forge_venue_freshness() {
        let mut control_plane = ControlPlane::default();

        mark_runtime_healthy(&mut control_plane, &config(), None);

        assert!(!control_plane
            .system
            .source_is_fresh_at("venue:polymarket", Utc::now()));
    }

    #[test]
    fn backoff_schedule_grows() {
        let first = next_live_reconcile_at(&config(), 1);
        let second = next_live_reconcile_at(&config(), 2);
        assert!(second >= first);
    }
}
