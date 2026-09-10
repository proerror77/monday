use crate::runtime_support::verify_persisted_trading_state_snapshot;
use crate::{restore_persisted_trading_runtime, PersistedTradingStateSnapshot, ProposalStore};
use ploy_operator_contracts::DeploymentRuntimeMode;
use ploy_platform::DeploymentRecord;
use portfolio_core::prediction::TradingRuntime;
use std::collections::{BTreeMap, BTreeSet};
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
    let mut seen_deployment_ids = BTreeSet::new();
    for snapshot in snapshots {
        verify_persisted_trading_state_snapshot(&snapshot)?;
        let deployment_id = snapshot.snapshot.deployment_id.clone();
        if !seen_deployment_ids.insert(deployment_id.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate persisted trading deployment identity `{deployment_id}`"),
            ));
        }
        let Some(expected_mode) = expected_runtime_mode(&snapshot.snapshot.deployment_id) else {
            continue;
        };
        if snapshot.snapshot.runtime_mode != expected_mode {
            continue;
        }
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
    use portfolio_core::prediction::{
        FillRecord, IntentPurpose, TradeSide, TradingIntent, TradingRuntime,
    };
    use rust_decimal_macros::dec;
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

    fn persisted_snapshot_with_multiple_fills(
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
            desired_state: ploy_operator_contracts::DesiredState::Running,
            observed_state: ploy_operator_contracts::ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-roundtrip".to_string(),
                    deployment_id: deployment_id.to_string(),
                    market_id: "market-roundtrip".to_string(),
                    token_id: "token-roundtrip".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-roundtrip",
                None,
            )
            .expect("roundtrip intent");
        runtime.acknowledge_order("order-roundtrip", "venue-roundtrip");
        for (fill_id, timestamp) in [("fill-roundtrip-a", 1), ("fill-roundtrip-b", 2)] {
            assert!(runtime.record_fill(FillRecord {
                fill_id: fill_id.to_string(),
                order_id: "order-roundtrip".to_string(),
                token_id: "token-roundtrip".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                price: dec!(0.5),
                fee: dec!(0.01),
                timestamp: chrono::DateTime::from_timestamp(timestamp, 0).expect("timestamp"),
            }));
        }
        serde_json::to_value(
            super::super::build_persisted_trading_state_snapshot(
                record,
                runtime.snapshot(&BTreeMap::new()),
            )
            .expect("multi-fill canonical test snapshot"),
        )
        .expect("serialize multi-fill canonical test snapshot")
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
    fn trading_runtime_load_rejects_missing_or_tampered_envelope_digest() {
        let mut missing_digest = persisted_snapshot("example.digest-missing", DeploymentRuntimeMode::Paper);
        missing_digest
            .as_object_mut()
            .expect("envelope object")
            .remove("integrity_digest");
        let missing_path = temp_path("trading-missing-digest");
        fs::write(
            &missing_path,
            serde_json::to_string(&vec![missing_digest]).expect("serialize missing digest"),
        )
        .expect("write missing digest");
        assert!(load_trading_runtimes(&missing_path, |_| Some(DeploymentRuntimeMode::Paper)).is_err());
        let _ = fs::remove_file(missing_path);

        let mut tampered = persisted_snapshot("example.digest-tampered", DeploymentRuntimeMode::Paper);
        tampered["snapshot"]["deployment_id"] = serde_json::json!("unknown.owner");
        let tampered_path = temp_path("trading-tampered-digest");
        fs::write(
            &tampered_path,
            serde_json::to_string(&vec![tampered]).expect("serialize tampered digest"),
        )
        .expect("write tampered digest");
        assert!(load_trading_runtimes(&tampered_path, |_| None).is_err());
        let _ = fs::remove_file(tampered_path);

        let mut tampered_mode = persisted_snapshot("example.digest-mode", DeploymentRuntimeMode::Paper);
        tampered_mode["snapshot"]["runtime_mode"] = serde_json::json!("live");
        let mode_path = temp_path("trading-tampered-mode");
        fs::write(
            &mode_path,
            serde_json::to_string(&vec![tampered_mode]).expect("serialize tampered mode"),
        )
        .expect("write tampered mode");
        assert!(load_trading_runtimes(&mode_path, |_| None).is_err());
        let _ = fs::remove_file(mode_path);

        let assert_tampered_rejected = |label: &str, value: serde_json::Value| {
            let path = temp_path(label);
            fs::write(
                &path,
                serde_json::to_string(&vec![value]).expect("serialize tampered envelope"),
            )
            .expect("write tampered envelope");
            assert!(
                load_trading_runtimes(&path, |_| None).is_err(),
                "{label} tampering must be rejected before deployment filtering"
            );
            let _ = fs::remove_file(path);
        };

        let mut tampered_intent = persisted_snapshot_with_multiple_fills(
            "example.digest-intent",
            DeploymentRuntimeMode::Paper,
        );
        tampered_intent["snapshot"]["intents"][0]["token_id"] = serde_json::json!("other-token");
        assert_tampered_rejected("trading-tampered-intent", tampered_intent);

        let mut tampered_fill = persisted_snapshot_with_multiple_fills(
            "example.digest-fill",
            DeploymentRuntimeMode::Paper,
        );
        tampered_fill["snapshot"]["fills"][0]["quantity"] = serde_json::json!("0.5");
        assert_tampered_rejected("trading-tampered-fill", tampered_fill);

        let mut tampered_inner_digest = persisted_snapshot_with_multiple_fills(
            "example.digest-inner",
            DeploymentRuntimeMode::Paper,
        );
        tampered_inner_digest["canonical_snapshot_digest"] = serde_json::json!("sha256:tampered");
        assert_tampered_rejected("trading-tampered-inner-digest", tampered_inner_digest);

        let mut tampered_exception = persisted_snapshot_with_multiple_fills(
            "example.digest-exception",
            DeploymentRuntimeMode::Paper,
        );
        tampered_exception["canonical_portfolio"]["reconciliation_exceptions"] =
            serde_json::json!([{"order_id": null, "reason": "tampered", "event": null}]);
        assert_tampered_rejected("trading-tampered-exception", tampered_exception);
    }

    #[test]
    fn trading_runtime_load_rejects_duplicate_deployment_identity() {
        let snapshots = vec![
            persisted_snapshot("example.duplicate", DeploymentRuntimeMode::Paper),
            persisted_snapshot("example.duplicate", DeploymentRuntimeMode::Paper),
        ];
        let path = temp_path("trading-duplicate-deployment");
        fs::write(&path, serde_json::to_string(&snapshots).expect("serialize duplicates"))
            .expect("write duplicates");
        let error = load_trading_runtimes(&path, |_| Some(DeploymentRuntimeMode::Paper))
            .expect_err("duplicate deployment identities must fail closed");
        assert!(error.to_string().contains("duplicate persisted trading deployment identity"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn trading_runtime_load_round_trips_serialized_multi_fill_envelope() {
        let path = temp_path("trading-multi-fill-roundtrip");
        fs::write(
            &path,
            serde_json::to_string(&vec![persisted_snapshot_with_multiple_fills(
                "example.multi-fill",
                DeploymentRuntimeMode::Paper,
            )])
            .expect("serialize multi-fill envelope"),
        )
        .expect("write multi-fill envelope");
        let runtimes = load_trading_runtimes(&path, |id| {
            (id == "example.multi-fill").then_some(DeploymentRuntimeMode::Paper)
        })
        .expect("multi-fill envelope remains restorable after JSON roundtrip");
        assert_eq!(
            runtimes
                .get("example.multi-fill")
                .expect("multi-fill runtime")
                .snapshot(&BTreeMap::new())
                .fills
                .len(),
            2
        );
        let _ = fs::remove_file(path);
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
