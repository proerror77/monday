#![cfg(feature = "feature-jsonl")]

use hft_cex_research_input::feature_matrix::{
    import_feature_dataset_for_reuse, read_feature_rows, FeatureDatasetManifest,
};

// Captured from the original collector implementation at 421e88c. These are
// format fixtures, not market data or scientific execution evidence.
const V3_BYTES: &[u8] = include_bytes!("fixtures/pit-v3.jsonl");
const V2_BYTES: &[u8] = include_bytes!("fixtures/pit-v2.jsonl");
const V3_HASH: &str = "27ee8c360b333cfd74ed2b1b485037da46aee1fd1edeeba0a3836364062dff26";
const V2_HASH: &str = "c80ed28ac361280f39264fb030570dbd686f380fb9e828eb72193dc3b51b21a3";

#[test]
fn v3_producer_bytes_keep_original_hash_metadata_and_float_bits() {
    assert_eq!(hft_cex_research_input::sha256(V3_BYTES), V3_HASH);
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("producer.jsonl");
    std::fs::write(&input, V3_BYTES).unwrap();
    let imported = import_feature_dataset_for_reuse(
        "data-frozen-pit",
        &input,
        &root.path().join("artifacts"),
        V3_BYTES.len() as u64,
    )
    .unwrap();
    assert_eq!(imported.manifest().artifact_sha256, V3_HASH);
    assert_eq!(
        std::fs::read(&imported.manifest().artifact_path).unwrap(),
        V3_BYTES
    );
    let rows = read_feature_rows(imported.manifest()).unwrap();
    assert_eq!(rows, imported.rows());
    assert_eq!(
        rows[0].features["lob_imbalance"].to_bits(),
        0.10000000000000002_f64.to_bits()
    );
    assert_eq!(
        rows[1].features["lob_imbalance"].to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(rows[0].features["tiny"].to_bits(), 1e-8_f64.to_bits());

    let golden: FeatureDatasetManifest =
        serde_json::from_str(include_str!("fixtures/pit-v3-manifest.json")).unwrap();
    let mut observed = imported.manifest().clone();
    assert!(observed.created_at >= golden.created_at);
    observed.created_at = golden.created_at;
    observed.artifact_path = observed.artifact_path.file_name().unwrap().into();
    assert_eq!(
        serde_json::to_string_pretty(&observed).unwrap() + "\n",
        include_str!("fixtures/pit-v3-manifest.json")
    );
}

#[test]
fn v2_history_reads_default_series_but_v3_import_and_reader_reject_missing_ids() {
    assert_eq!(hft_cex_research_input::sha256(V2_BYTES), V2_HASH);
    let root = tempfile::tempdir().unwrap();
    let mut manifest: FeatureDatasetManifest =
        serde_json::from_str(include_str!("fixtures/pit-v2-manifest.json")).unwrap();
    assert_eq!(manifest.series_count, 1);
    manifest.artifact_path = root.path().join(format!("{V2_HASH}.jsonl"));
    std::fs::write(&manifest.artifact_path, V2_BYTES).unwrap();
    let rows = read_feature_rows(&manifest).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.series_id == 1));
    assert_eq!(
        rows[0].features["lob_imbalance"].to_bits(),
        0.10000000000000002_f64.to_bits()
    );
    assert_eq!(
        rows[1].features["lob_imbalance"].to_bits(),
        (-0.0_f64).to_bits()
    );

    let output = root.path().join("rejected-import");
    let error = import_feature_dataset_for_reuse(
        "data-frozen-pit",
        &manifest.artifact_path,
        &output,
        V2_BYTES.len() as u64,
    )
    .err()
    .unwrap();
    assert_eq!(error, "feature row 1 is missing series_id");
    assert!(!output.exists());
    manifest.schema_version = "pit-feature-matrix-v3".into();
    assert_eq!(read_feature_rows(&manifest).unwrap_err(), error);
    manifest.schema_version = "pit-feature-matrix-v2".into();
    std::fs::write(&manifest.artifact_path, [V2_BYTES, b"\n"].concat()).unwrap();
    assert_eq!(
        read_feature_rows(&manifest).unwrap_err(),
        "feature artifact does not match its content-addressed manifest"
    );
}
