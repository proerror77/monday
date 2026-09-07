use hft_research_ml::{
    load_contract_model_bundle, train_contract_model, ContractDatasetBinding,
    ContractModelBundleManifest, ContractTrainingConfig, ContractTrainingRow, FeatureName,
    PositiveDurationMs, PurgedWalkForwardSplit, SealedTrainingRequest, Sha256Digest, SplitId,
    SplitRole, Symbol, TimestampMs, TrainingRequest, Venue,
};
use std::path::{Path, PathBuf};

fn training_rows() -> Vec<ContractTrainingRow> {
    (0..16)
        .map(|index| ContractTrainingRow {
            observed_at_ms: TimestampMs::new(index * 1_000),
            feature_max_available_at_ms: TimestampMs::new(index * 1_000),
            label_available_at_ms: TimestampMs::new(index * 1_000 + 5_000),
            features: vec![index as f32 / 16.0],
            forward_return: (index as f32 - 8.0) / 10_000.0,
        })
        .collect()
}

fn dataset_binding() -> ContractDatasetBinding {
    ContractDatasetBinding::new(
        Sha256Digest::of_bytes(b"dataset-manifest"),
        Sha256Digest::of_bytes(b"feature-manifest"),
        Sha256Digest::of_bytes(b"label-manifest"),
        vec![FeatureName::new("return_1s").unwrap()],
        Symbol::new("BTCUSDT").unwrap(),
        Venue::new("binance-usdm").unwrap(),
        PositiveDurationMs::new(5_000).unwrap(),
    )
    .unwrap()
}

fn training_config() -> ContractTrainingConfig {
    ContractTrainingConfig {
        input_dim: 1,
        hidden_dim: 4,
        epochs: 8,
        learning_rate: 0.01,
        min_rows: 8,
        seed: 42,
    }
}

fn sealed_request(rows_artifact: &[u8]) -> (Vec<u8>, Sha256Digest, SealedTrainingRequest) {
    let split = PurgedWalkForwardSplit::new(
        SplitId::new("walk-forward-fold-0-train").unwrap(),
        SplitRole::Train,
        TimestampMs::new(0),
        TimestampMs::new(30_000),
        TimestampMs::new(35_000),
        PositiveDurationMs::new(5_000).unwrap(),
        PositiveDurationMs::new(5_000).unwrap(),
    )
    .unwrap();
    let request = TrainingRequest::new(
        Sha256Digest::of_bytes(rows_artifact),
        dataset_binding(),
        split,
        training_config(),
    )
    .unwrap();
    let bytes = serde_json::to_vec_pretty(&request).unwrap();
    let digest = Sha256Digest::of_bytes(&bytes);
    let sealed = SealedTrainingRequest::from_bytes(&bytes, &digest).unwrap();
    (bytes, digest, sealed)
}

fn write_content_addressed_manifest(
    output: &Path,
    manifest: &ContractModelBundleManifest,
) -> (PathBuf, Sha256Digest) {
    let bytes = serde_json::to_vec_pretty(manifest).unwrap();
    let digest = Sha256Digest::of_bytes(&bytes);
    let path = output.join(format!("contract-model-{digest}.json"));
    std::fs::write(&path, bytes).unwrap();
    (path, digest)
}

#[test]
fn sealed_request_rejects_the_wrong_external_digest() {
    let request_bytes = br#"{"schema_version":1}"#;
    let wrong = Sha256Digest::of_bytes(b"different request");

    let error = SealedTrainingRequest::from_bytes(request_bytes, &wrong).unwrap_err();

    assert!(error.to_string().contains("training request checksum"));
}

#[test]
fn training_rejects_rows_that_do_not_match_the_sealed_request() {
    let mut rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    rows_artifact.push(b'\n');

    let error = train_contract_model(&rows_artifact, &sealed).unwrap_err();

    assert!(error.to_string().contains("rows artifact checksum"));
}

#[test]
fn training_rejects_features_that_were_not_available_at_observation() {
    let mut rows = training_rows();
    rows[0].feature_max_available_at_ms = TimestampMs::new(1);
    let rows_artifact = serde_json::to_vec(&rows).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);

    let error = train_contract_model(&rows_artifact, &sealed).unwrap_err();

    assert!(error.to_string().contains("feature availability"));
}

#[test]
fn training_rejects_labels_available_before_the_prediction_horizon() {
    let mut rows = training_rows();
    rows[0].label_available_at_ms = TimestampMs::new(4_999);
    let rows_artifact = serde_json::to_vec(&rows).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);

    let error = train_contract_model(&rows_artifact, &sealed).unwrap_err();

    assert!(error.to_string().contains("prediction horizon"));
}

#[test]
fn independent_training_runs_have_the_same_semantic_digests() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);

    let first = train_contract_model(&rows_artifact, &sealed).unwrap();
    let second = train_contract_model(&rows_artifact, &sealed).unwrap();

    assert_eq!(
        first.diagnostics().semantic_model_sha256,
        second.diagnostics().semantic_model_sha256
    );
    assert_eq!(
        first.diagnostics().request_semantic_sha256,
        second.diagnostics().request_semantic_sha256
    );
    assert_eq!(
        first.predict(&[0.25]).unwrap().to_bits(),
        second.predict(&[0.25]).unwrap().to_bits()
    );
}

#[test]
fn concurrent_training_runs_share_one_backend_without_digest_drift() {
    let rows_artifact = std::sync::Arc::new(serde_json::to_vec(&training_rows()).unwrap());
    let (_, _, sealed) = sealed_request(rows_artifact.as_slice());
    let sealed = std::sync::Arc::new(sealed);
    let joins = (0..8)
        .map(|_| {
            let rows_artifact = rows_artifact.clone();
            let sealed = sealed.clone();
            std::thread::spawn(move || {
                let trained =
                    train_contract_model(rows_artifact.as_slice(), sealed.as_ref()).unwrap();
                (
                    trained.diagnostics().semantic_model_sha256.clone(),
                    trained.predict(&[0.25]).unwrap().to_bits(),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = joins
        .into_iter()
        .map(|join| join.join().expect("training thread"))
        .collect::<Vec<_>>();
    for result in &results[1..] {
        assert_eq!(result, &results[0]);
    }
}

#[test]
fn fit_diagnostics_have_no_promotion_capability() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();

    let diagnostics = serde_json::to_value(trained.diagnostics()).unwrap();

    assert_eq!(diagnostics["authority"], "fit_diagnostics_only");
    assert!(diagnostics.get("promotion_eligible").is_none());
    assert!(diagnostics.get("evaluation_scope").is_none());
}

#[test]
fn bundle_publication_is_idempotent_and_loading_requires_the_expected_digest() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();
    let output = tempfile::tempdir().unwrap();

    let first = trained.save_bundle(output.path()).unwrap();
    let second = trained.save_bundle(output.path()).unwrap();

    assert_eq!(first.model_path, second.model_path);
    assert_eq!(first.manifest_path, second.manifest_path);
    assert_eq!(first.manifest_sha256, second.manifest_sha256);
    let loaded = load_contract_model_bundle(&first.manifest_path, &first.manifest_sha256).unwrap();
    assert_eq!(
        loaded.diagnostics().semantic_model_sha256,
        trained.diagnostics().semantic_model_sha256
    );
    let wrong_digest = Sha256Digest::of_bytes(b"wrong manifest");
    let error = load_contract_model_bundle(&first.manifest_path, &wrong_digest).unwrap_err();
    assert!(error.to_string().contains("externally expected"));
}

#[test]
fn loading_rejects_an_unsafe_model_basename_even_when_the_manifest_digest_matches() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();
    let output = tempfile::tempdir().unwrap();
    let saved = trained.save_bundle(output.path()).unwrap();
    let mut tampered = saved.manifest;
    tampered.model_file = "../outside.bpk".to_string();
    let (manifest_path, manifest_digest) =
        write_content_addressed_manifest(output.path(), &tampered);

    let error = load_contract_model_bundle(&manifest_path, &manifest_digest).unwrap_err();

    assert!(error
        .to_string()
        .contains("safe content-addressed basename"));
}

#[test]
fn loading_rejects_manifest_fields_detached_from_the_sealed_request() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();
    let output = tempfile::tempdir().unwrap();
    let saved = trained.save_bundle(output.path()).unwrap();
    let mut tampered = saved.manifest;
    tampered.diagnostics.config_sha256 = Sha256Digest::of_bytes(b"different config");
    let (manifest_path, manifest_digest) =
        write_content_addressed_manifest(output.path(), &tampered);

    let error = load_contract_model_bundle(&manifest_path, &manifest_digest).unwrap_err();

    assert!(error.to_string().contains("internal consistency"));
}

#[test]
fn loading_rejects_tampered_burnpack_bytes() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();
    let output = tempfile::tempdir().unwrap();
    let saved = trained.save_bundle(output.path()).unwrap();
    std::fs::write(&saved.model_path, b"tampered").unwrap();

    let error =
        load_contract_model_bundle(&saved.manifest_path, &saved.manifest_sha256).unwrap_err();

    assert!(error.to_string().contains("model artifact checksum"));
}

#[test]
fn cas_publication_never_overwrites_a_preexisting_mismatched_target() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let (_, _, sealed) = sealed_request(&rows_artifact);
    let trained = train_contract_model(&rows_artifact, &sealed).unwrap();
    let source = tempfile::tempdir().unwrap();
    let reference = trained.save_bundle(source.path()).unwrap();
    let output = tempfile::tempdir().unwrap();
    let occupied = output
        .path()
        .join(reference.model_path.file_name().unwrap());
    std::fs::write(&occupied, b"preexisting mismatch").unwrap();

    let error = trained.save_bundle(output.path()).unwrap_err();

    assert!(error.to_string().contains("different bytes"));
    assert_eq!(std::fs::read(occupied).unwrap(), b"preexisting mismatch");
}

#[test]
fn training_request_rejects_non_training_roles_and_short_embargoes() {
    let rows_artifact = serde_json::to_vec(&training_rows()).unwrap();
    let validation_split = PurgedWalkForwardSplit::new(
        SplitId::new("fold-0-validation").unwrap(),
        SplitRole::Validation,
        TimestampMs::new(0),
        TimestampMs::new(30_000),
        TimestampMs::new(35_000),
        PositiveDurationMs::new(5_000).unwrap(),
        PositiveDurationMs::new(5_000).unwrap(),
    )
    .unwrap();
    let role_error = TrainingRequest::new(
        Sha256Digest::of_bytes(&rows_artifact),
        dataset_binding(),
        validation_split,
        training_config(),
    )
    .unwrap_err();
    assert!(role_error.to_string().contains("role must be train"));

    let short_embargo_split = PurgedWalkForwardSplit::new(
        SplitId::new("fold-0-train").unwrap(),
        SplitRole::Train,
        TimestampMs::new(0),
        TimestampMs::new(30_000),
        TimestampMs::new(35_000),
        PositiveDurationMs::new(5_000).unwrap(),
        PositiveDurationMs::new(4_999).unwrap(),
    )
    .unwrap();
    let embargo_error = TrainingRequest::new(
        Sha256Digest::of_bytes(&rows_artifact),
        dataset_binding(),
        short_embargo_split,
        training_config(),
    )
    .unwrap_err();
    assert!(embargo_error
        .to_string()
        .contains("at least the prediction horizon"));
}
