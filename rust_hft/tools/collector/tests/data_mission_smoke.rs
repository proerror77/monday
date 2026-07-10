use hft_collector::{acquire_dataset, DataAcquisitionMission};
use sha2::{Digest, Sha256};

#[tokio::test]
#[ignore = "requires public Binance network access"]
async fn real_binance_data_mission_produces_manifest_and_quality_report() {
    let artifact_dir = std::env::temp_dir().join("alpha-real-binance-data-mission");
    let manifest = acquire_dataset(
        &DataAcquisitionMission {
            mission_id: "real-binance-smoke".to_string(),
            source_id: "binance-public".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            limit: 5,
        },
        artifact_dir,
    )
    .await
    .unwrap();
    assert_eq!(manifest.quality.rows, 5);
    assert_eq!(manifest.quality.non_finite_values, 0);
    assert!(manifest.artifact_path.exists());
    assert!(manifest.manifest_id.ends_with(&manifest.artifact_sha256));
    let bytes = std::fs::read(&manifest.artifact_path).unwrap();
    assert_eq!(hex::encode(Sha256::digest(bytes)), manifest.artifact_sha256);
}
