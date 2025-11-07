use std::collections::HashSet;

use hft_core::latency::{LatencyStage, LatencyStats, LatencyTracker};

#[test]
fn core_stage_sequence_is_unique_and_ordered() {
    let stages = LatencyStage::core_stages();
    assert_eq!(stages.len(), 8);

    let mut seen = HashSet::new();
    for stage in stages {
        assert!(seen.insert(stage), "duplicate stage {:?}", stage);
    }

    let labels: HashSet<_> = LatencyStage::core_stages()
        .iter()
        .map(|stage| stage.as_str())
        .collect();
    assert_eq!(labels.len(), 8);
}

#[test]
fn tracker_records_new_stages() {
    let mut tracker = LatencyTracker::new();
    for stage in LatencyStage::core_stages() {
        tracker.record_stage(stage);
    }

    let measurements = tracker.get_all_measurements();
    // 核心 8 階段 + EndToEnd
    assert_eq!(measurements.len(), 9);
    assert!(measurements
        .iter()
        .any(|m| matches!(m.stage, LatencyStage::EndToEnd)));
}

#[test]
fn stats_collects_each_stage() {
    let mut tracker = LatencyTracker::new();
    // 模擬資料，依序記錄 8 個階段
    for stage in LatencyStage::core_stages() {
        tracker.record_stage(stage);
    }

    let mut stats = LatencyStats::new();
    stats.add_tracker(&tracker);

    for stage in LatencyStage::core_stages() {
        let stage_stats = stats.get_stage_stats(stage);
        // 目前每個階段都應至少有 1 個樣本
        assert_eq!(
            stage_stats.count, 1,
            "stage {:?} expected sample, got {:?}",
            stage, stage_stats
        );
    }

    let end_to_end_stats = stats.get_stage_stats(LatencyStage::EndToEnd);
    assert_eq!(end_to_end_stats.count, 1);
}
