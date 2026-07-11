use std::path::Path;

use alpha_domain::StrategyBundle;
use chrono::Utc;
use hft_live::deployment_envelope::{
    ActivationArtifact, ActivationMode, ActivationRequest, RuntimeActivationAdapter,
    SystemConfigActivationAdapter,
};
use runtime::SystemBuilder;
use serde::Deserialize;

const K8S_CONFIG_MAPS: &str = include_str!("../../../deployment/k8s/configmaps.yaml");
const PRODUCTION_CONFIG: &str = include_str!("../../../config/prod/system.yaml");

fn formula_bundle() -> StrategyBundle {
    let mut bundle: StrategyBundle = serde_json::from_value(serde_json::json!({
        "bundle_id": "bundle-1",
        "candidate_id": "candidate-1",
        "candidate_content_hash": "1".repeat(64),
        "dataset_manifest_id": "dataset-1",
        "evaluator_version": "sealed-holdout-v1",
        "sealed_evaluation_hash": "2".repeat(64),
        "artifact": {"Formula": {"ast": {"Terminal": {"Field": "book_imbalance"}}}},
        "bundle_hash": "",
        "created_at": Utc::now(),
    }))
    .unwrap();
    bundle.bundle_hash = bundle.calculated_hash().unwrap();
    bundle
}

#[test]
fn production_config_is_parseable_and_fail_closed() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/prod/system.yaml");
    let builder = SystemBuilder::from_yaml(path.to_str().expect("utf-8 config path"))
        .expect("production config must match the current runtime schema");
    let config = builder.config();

    assert!(config.quotes_only);
    assert!(config.strategies.is_empty());
    assert_eq!(config.venues.len(), 1);
    assert_eq!(config.venues[0].account_id.as_deref(), Some("bitget_main"));
    assert!(config.venues[0].simulate_execution);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Paper"));
}

#[test]
fn production_config_accepts_the_governed_paper_target() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/prod/system.yaml");
    let mut config = SystemBuilder::from_yaml(path.to_str().expect("utf-8 config path"))
        .expect("production config")
        .config()
        .clone();
    let bundle = formula_bundle();
    let request = ActivationRequest {
        deployment_id: "deployment-1".to_string(),
        asset_revision_id: bundle.candidate_id.clone(),
        promotion_id: "promotion-1".to_string(),
        bundle_id: bundle.bundle_id.clone(),
        bundle_hash: bundle.bundle_hash.clone(),
        account_id: "bitget_main".to_string(),
        venue: "bitget".to_string(),
        instruments: vec!["BTCUSDT".to_string()],
        artifact: ActivationArtifact::Formula,
        mode: ActivationMode::Paper,
        max_notional: 1_000.0,
        max_symbol_exposure: 500.0,
        max_order_size: 100.0,
        max_slippage_bps: 10.0,
    };

    SystemConfigActivationAdapter::new(&mut config, &bundle, Path::new("bundle.json"))
        .activate(&request)
        .expect("canonical deployment target must resolve");
    assert!(!config.quotes_only);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Paper"));
    SystemBuilder::new(config)
        .auto_register_adapters_strict()
        .expect("paper target must have market, execution, and strategy implementations");
}

#[test]
fn kubernetes_embeds_the_canonical_production_config() {
    let document = serde_yaml::Deserializer::from_str(K8S_CONFIG_MAPS)
        .next()
        .expect("configmap document");
    let manifest = serde_yaml::Value::deserialize(document).expect("valid configmap yaml");
    let embedded = manifest
        .get("data")
        .and_then(|value| value.get("system.yaml"))
        .and_then(serde_yaml::Value::as_str)
        .expect("system.yaml entry");

    let canonical: serde_yaml::Value =
        serde_yaml::from_str(PRODUCTION_CONFIG).expect("canonical production config");
    let embedded: serde_yaml::Value =
        serde_yaml::from_str(embedded).expect("embedded production config");
    assert_eq!(embedded, canonical);
}
