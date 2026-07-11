use hft_collector::{acquire_dataset, CandleInterval, DataAcquisitionMission, QualityRequirements};
use sha2::{Digest, Sha256};

#[tokio::test]
#[ignore = "requires public Binance network access"]
async fn real_binance_data_mission_produces_manifest_and_quality_report() {
    let artifact_dir = std::env::temp_dir().join("alpha-real-binance-data-mission");
    let manifest = acquire_dataset(&DataAcquisitionMission {
        mission_id: "real-binance-smoke".to_string(),
        source_id: "binance-public".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        limit: 5,
        artifact_dir,
        quality_requirements: QualityRequirements {
            max_parse_failures: 0,
            max_non_monotonic_events: 0,
            max_non_finite_values: 0,
        },
    })
    .await
    .unwrap();
    assert_eq!(manifest.quality.rows, 5);
    assert_eq!(manifest.quality.non_finite_values, 0);
    assert_eq!(manifest.interval, CandleInterval::OneMinute);
    assert_eq!(manifest.quality.duplicate_timestamps, 0);
    assert_eq!(manifest.quality.interval_gaps, 0);
    assert_eq!(manifest.quality.open_or_partial_candles, 0);
    assert!(!manifest.quality.stale);
    assert!(manifest.time_bounds.last_exchange_time < manifest.created_at);
    assert!(manifest.artifact_path.exists());
    assert!(manifest.manifest_id.ends_with(&manifest.artifact_sha256));
    let bytes = std::fs::read(&manifest.artifact_path).unwrap();
    assert_eq!(hex::encode(Sha256::digest(bytes)), manifest.artifact_sha256);
}
