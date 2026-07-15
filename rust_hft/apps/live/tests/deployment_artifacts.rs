use std::{collections::BTreeMap, path::Path};

#[cfg(all(feature = "formula-strategy", feature = "bitget"))]
use alpha_domain::{StrategyBundle, SEALED_HOLDOUT_EVALUATOR_VERSION};
#[cfg(all(feature = "formula-strategy", feature = "bitget"))]
use chrono::Utc;
#[cfg(all(feature = "formula-strategy", feature = "bitget"))]
use hft_live::deployment_envelope::{
    ActivationArtifact, ActivationMode, ActivationRequest, RuntimeActivationAdapter,
    SystemConfigActivationAdapter,
};
use runtime::SystemBuilder;
use serde::Deserialize;

const K8S_CONFIG_MAPS: &str = include_str!("../../../deployment/k8s/configmaps.yaml");
const K8S_TRADING_ENGINE: &str = include_str!("../../../deployment/k8s/trading-engine.yaml");
const PRODUCTION_CONFIG: &str = include_str!("../../../config/prod/system.yaml");
const RUST_HFT_DOCKERFILE: &str = include_str!("../../../docker/Dockerfile");
const TRADING_DOCKERFILE: &str = include_str!("../../../deployment/docker/Dockerfile.trading");
const ROOT_DOCKERFILE: &str = include_str!("../../../../deploy/Dockerfile.hft");
const ROOT_COMPOSE: &str = include_str!("../../../../deploy/docker-compose.yml");
const ROOT_PROMETHEUS: &str = include_str!("../../../../deploy/prometheus/prometheus.yml");
const TRADING_COMPOSE: &str = include_str!("../../../deployment/docker/docker-compose.yml");
const DEPLOY_SCRIPT: &str = include_str!("../../../deployment/scripts/deploy.sh");

fn assert_text_contract(label: &str, content: &str, required: &[&str], forbidden: &[&str]) {
    for needle in required {
        assert!(
            content.contains(needle),
            "{label} must contain required deployment contract text {needle:?}"
        );
    }
    for needle in forbidden {
        assert!(
            !content.contains(needle),
            "{label} must not contain forbidden deployment contract text {needle:?}"
        );
    }
}

fn yaml_scalar(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn published_host_binding(port: &serde_yaml::Value) -> Option<String> {
    match port {
        serde_yaml::Value::String(short) => {
            let without_protocol = short.split('/').next().unwrap_or(short);
            let mut segments = without_protocol.rsplitn(3, ':');
            let _target = segments.next()?;
            let published = segments.next()?;
            let host_ip = segments.next().unwrap_or_default();
            Some(format!("{host_ip}:{published}"))
        }
        serde_yaml::Value::Mapping(long) => {
            let published = long
                .get(serde_yaml::Value::String("published".to_string()))
                .and_then(yaml_scalar)?;
            let host_ip = long
                .get(serde_yaml::Value::String("host_ip".to_string()))
                .and_then(yaml_scalar)
                .unwrap_or_default();
            Some(format!("{host_ip}:{published}"))
        }
        _ => None,
    }
}

fn duplicate_published_host_ports(compose: &str) -> Vec<String> {
    let manifest: serde_yaml::Value =
        serde_yaml::from_str(compose).expect("compose file must be valid YAML");
    let services = manifest
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("compose file must define services");
    let mut binding_counts = BTreeMap::<String, usize>::new();

    for service in services.values() {
        let Some(ports) = service
            .get("ports")
            .and_then(serde_yaml::Value::as_sequence)
        else {
            continue;
        };
        for port in ports {
            if let Some(binding) = published_host_binding(port) {
                *binding_counts.entry(binding).or_default() += 1;
            }
        }
    }

    binding_counts
        .into_iter()
        .filter_map(|(binding, count)| (count > 1).then_some(binding))
        .collect()
}

#[cfg(all(feature = "formula-strategy", feature = "bitget"))]
fn formula_bundle() -> StrategyBundle {
    let mut bundle: StrategyBundle = serde_json::from_value(serde_json::json!({
        "bundle_id": "bundle-1",
        "candidate_id": "candidate-1",
        "candidate_content_hash": "1".repeat(64),
        "dataset_manifest_id": "dataset-1",
        "evaluator_version": SEALED_HOLDOUT_EVALUATOR_VERSION,
        "evaluation_protocol_hash": "5".repeat(64),
        "evaluator_config_hash": "3".repeat(64),
        "evaluation_metrics_hash": "4".repeat(64),
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
#[cfg(all(feature = "formula-strategy", feature = "bitget"))]
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
    assert_eq!(
        config.engine.intent_max_order_quantity,
        Some(rust_decimal::Decimal::from(100))
    );
    assert_eq!(
        config.engine.intent_max_order_notional,
        Some(rust_decimal::Decimal::from(500))
    );
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

#[test]
fn docker_artifacts_enforce_the_live_runtime_contract() {
    let runtime_image_required = [
        "-p hft-live",
        "target/release/hft-live",
        "curl",
        "protobuf-compiler",
        "--mount=type=cache,target=/usr/local/cargo/registry",
        "/readiness",
        "ENTRYPOINT [\"/usr/local/bin/hft-live\"]",
        "EXPOSE 9090 9092",
        "USER hft",
        "clickhouse,redis,grpc",
    ];
    let runtime_image_forbidden = ["hft-collector", "EXPOSE 9090 9091 9092", "|| true"];

    for (label, content) in [
        ("rust_hft/docker/Dockerfile", RUST_HFT_DOCKERFILE),
        (
            "rust_hft/deployment/docker/Dockerfile.trading",
            TRADING_DOCKERFILE,
        ),
    ] {
        assert_text_contract(
            label,
            content,
            &runtime_image_required,
            &runtime_image_forbidden,
        );
    }

    assert_text_contract(
        "deploy/Dockerfile.hft",
        ROOT_DOCKERFILE,
        &[
            "curl",
            "http://localhost:9090/readiness",
            "EXPOSE 9090 9092",
            "USER hft",
        ],
        &["EXPOSE 9090 9091 9092"],
    );
    assert_text_contract(
        "deploy/docker-compose.yml",
        ROOT_COMPOSE,
        &["19090:9090"],
        &["command: [\"hft-live\"", "command: [\"hft-paper\""],
    );
    assert_text_contract(
        "deploy/prometheus/prometheus.yml",
        ROOT_PROMETHEUS,
        &["trader:9090"],
        &["trader:8080"],
    );

    let compose_required = [
        "--strategy-bundle",
        "--deployment-feedback-signing-key",
        "--deployment-feedback-key-id",
        "/run/secrets/hft/feedback-signing-key.hex",
    ];
    for (label, content) in [
        ("deploy/docker-compose.yml", ROOT_COMPOSE),
        (
            "rust_hft/deployment/docker/docker-compose.yml",
            TRADING_COMPOSE,
        ),
    ] {
        assert_text_contract(label, content, &compose_required, &[]);
    }
}

#[test]
fn kubernetes_artifacts_enforce_the_live_runtime_contract() {
    assert_text_contract(
        "rust_hft/deployment/k8s/trading-engine.yaml",
        K8S_TRADING_ENGINE,
        &[
            "path: /readiness",
            "--deployment-envelope",
            "--strategy-bundle",
            "--deployment-policy",
            "--deployment-trusted-keys",
            "--deployment-nonce-ledger",
            "--deployment-audit-log",
            "--deployment-feedback-log",
            "--deployment-feedback-signing-key",
            "--deployment-feedback-key-id",
            "/app/deployment/envelope.json",
            "/app/deployment/bundle.json",
            "/app/deployment/policy.json",
            "/app/deployment/trusted-keys.json",
            "/app/state/nonces.jsonl",
            "/app/state/audit.jsonl",
            "/app/state/feedback.jsonl",
            "/app/secrets/feedback-signing-key.hex",
            "claimName: runtime-state-pvc",
            "prometheus.io/port: \"9090\"",
            "HFT_GRPC_AUTH_TOKEN",
            "key: grpc-auth-token",
            "key: feedback-signing-key-hex",
        ],
        &["path: /ready", "containerPort: 9091", "BITGET_API_SECRET"],
    );
    assert_text_contract(
        "rust_hft/deployment/k8s/configmaps.yaml",
        K8S_CONFIG_MAPS,
        &[
            "system.yaml: |",
            "quotes_only: true",
            "simulate_execution: true",
        ],
        &[],
    );
}

#[test]
fn deployment_script_requires_external_authority_and_secrets() {
    assert_text_contract(
        "rust_hft/deployment/scripts/deploy.sh",
        DEPLOY_SCRIPT,
        &[
            "kubectl apply -f \"$K8S_DIR/configmaps.yaml\"",
            "HFT_K8S_DEPLOYMENT_ENVELOPE_FILE",
            "HFT_K8S_DEPLOYMENT_AUTHORITY_FILE",
            "require_configmap_key alpha-deployment-envelope envelope.json",
            "require_configmap_key alpha-deployment-envelope bundle.json",
            "require_secret_key hft-secrets grpc-auth-token",
            "require_secret_key hft-secrets feedback-signing-key-hex",
        ],
        &["envsubst < \"$K8S_DIR/configmaps.yaml\""],
    );
}

#[test]
fn compose_files_have_unique_published_host_ports() {
    for (label, compose) in [
        ("deploy/docker-compose.yml", ROOT_COMPOSE),
        (
            "rust_hft/deployment/docker/docker-compose.yml",
            TRADING_COMPOSE,
        ),
    ] {
        let duplicates = duplicate_published_host_ports(compose);
        assert!(
            duplicates.is_empty(),
            "{label} publishes duplicate host ports: {}",
            duplicates.join(", ")
        );
    }
}

#[test]
fn duplicate_compose_published_host_ports_are_detected() {
    let compose = r#"
services:
  trader:
    ports:
      - "19090:9090"
  metrics:
    ports:
      - target: 9090
        published: 19090
"#;

    assert_eq!(duplicate_published_host_ports(compose), [":19090"]);
}
