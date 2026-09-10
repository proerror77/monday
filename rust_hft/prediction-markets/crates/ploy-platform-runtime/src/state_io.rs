use crate::{restore_persisted_trading_runtime, PersistedTradingStateSnapshot, ProposalStore};
use ploy_operator_contracts::DeploymentRuntimeMode;
use ploy_platform::DeploymentRecord;
use portfolio_core::prediction::TradingRuntime;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub fn load_registry_records(path: &Path) -> io::Result<Vec<DeploymentRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&raw).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub fn load_trading_runtimes(
    path: &Path,
    expected_runtime_mode: impl Fn(&str) -> Option<DeploymentRuntimeMode>,
) -> io::Result<BTreeMap<String, TradingRuntime>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let snapshots: Vec<PersistedTradingStateSnapshot> = serde_json::from_str(&raw)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    let mut runtimes = BTreeMap::new();
    for snapshot in snapshots {
        let Some(expected_mode) = expected_runtime_mode(&snapshot.snapshot.deployment_id) else {
            continue;
        };
        if snapshot.snapshot.runtime_mode != expected_mode {
            continue;
        }
        let deployment_id = snapshot.snapshot.deployment_id.clone();
        runtimes.insert(deployment_id, restore_persisted_trading_runtime(snapshot)?);
    }

    Ok(runtimes)
}

pub fn load_proposal_store(path: &Path) -> io::Result<ProposalStore> {
    if !path.exists() {
        return Ok(ProposalStore::default());
    }

    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(ProposalStore::default());
    }

    let proposals = serde_json::from_str(&raw)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut store = ProposalStore::default();
    store.replace(proposals);
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::{load_proposal_store, load_registry_records, load_trading_runtimes};
    use chrono::Utc;
    use ploy_operator_contracts::DeploymentRuntimeMode;
    use ploy_platform::DeploymentRecord;
    use portfolio_core::prediction::TradingRuntime;
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ploy-state-io-{label}-{unique}.json"))
    }

    fn persisted_snapshot(
        deployment_id: &str,
        runtime_mode: DeploymentRuntimeMode,
    ) -> serde_json::Value {
        let record = DeploymentRecord {
            deployment_id: deployment_id.to_string(),
            bundle_id: "example".to_string(),
            runtime_mode,
            account_id: "acct-test".to_string(),
            max_gross_exposure: None,
            deployment_state: ploy_operator_contracts::DeploymentState::Enabled,
            desired_state: ploy_operator_contracts::DesiredState::Paused,
            observed_state: ploy_operator_contracts::ObservedState::Paused,
        };
        let snapshot = TradingRuntime::default().snapshot(&BTreeMap::new());
        serde_json::to_value(
            super::super::build_persisted_trading_state_snapshot(record, snapshot)
                .expect("canonical test snapshot"),
        )
        .expect("serialize canonical test snapshot")
    }

    #[test]
    fn loads_registry_records() {
        let path = temp_path("registry");
        fs::write(
            &path,
            serde_json::json!([{
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "acct-paper",
                "max_gross_exposure": "5.00",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "running"
            }])
            .to_string(),
        )
        .expect("write");
        let records = load_registry_records(&path).expect("load");
        assert_eq!(records.len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn loads_trading_runtimes_for_known_deployments() {
        let path = temp_path("trading");
        fs::write(
            &path,
            serde_json::to_string(&vec![persisted_snapshot(
                "example.paper",
                DeploymentRuntimeMode::Paper,
            )])
            .expect("serialize"),
        )
        .expect("write");
        let runtimes = load_trading_runtimes(&path, |id| {
            (id == "example.paper").then_some(ploy_operator_contracts::DeploymentRuntimeMode::Paper)
        })
        .expect("load");
        assert_eq!(runtimes.len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn trading_runtime_load_requires_snapshot_mode_to_match_registry_mode() {
        for (snapshot_mode, registry_mode, expected) in [
            (
                "paper",
                ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                1,
            ),
            (
                "live",
                ploy_operator_contracts::DeploymentRuntimeMode::Live,
                1,
            ),
            (
                "paper",
                ploy_operator_contracts::DeploymentRuntimeMode::Live,
                0,
            ),
            (
                "live",
                ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                0,
            ),
        ] {
            let path = temp_path("trading-mode-match");
            let snapshot_mode = match snapshot_mode {
                "paper" => DeploymentRuntimeMode::Paper,
                "live" => DeploymentRuntimeMode::Live,
                _ => unreachable!(),
            };
            fs::write(
                &path,
                serde_json::to_string(&vec![persisted_snapshot("example.mode", snapshot_mode)])
                    .expect("serialize"),
            )
            .expect("snapshot");
            let runtimes = load_trading_runtimes(&path, |_| Some(registry_mode.clone()))
                .expect("load mode-aware runtimes");
            assert_eq!(runtimes.len(), expected);
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn loads_proposal_store() {
        let path = temp_path("proposals");
        fs::write(
            &path,
            serde_json::json!([{
                "proposal_id": "proposal-1",
                "action_kind": "pause_deployment",
                "target_deployment_id": "example.paper",
                "status": "pending",
                "rationale": "drift",
                "evidence": ["drawdown"],
                "source_run_id": null,
                "proposed_max_gross_exposure": null,
                "created_at": Utc::now(),
                "decided_at": null,
                "decision_note": null
            }])
            .to_string(),
        )
        .expect("write");
        let store = load_proposal_store(&path).expect("load");
        assert_eq!(store.all().len(), 1);
        let _ = fs::remove_file(path);
    }
}
