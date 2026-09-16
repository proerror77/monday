use super::*;
use std::collections::BTreeMap;

fn fixture(
    predictions: &[f64],
    labels: &[f64],
) -> (
    Vec<ResearchRow>,
    Vec<PositionEvaluationPoint>,
    EvaluationProtocolV1,
    CexSupervisedDecisionPolicyV2,
) {
    let policy = CexSupervisedDecisionPolicyV2::hold_to_horizon_v3(5000).unwrap();
    let protocol = EvaluationProtocolV1::new(
        alpha_domain::EvaluationWalkForwardV1 {
            initial_train_rows: 60,
            validation_rows: 60,
            fold_count: 2,
            purge_rows: 5,
            embargo_rows: 5,
            sealed_holdout_rows: 60,
        },
        alpha_domain::EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
        },
        alpha_domain::EvaluationLabelSpecV1 {
            horizon_buckets: 5,
            observation_frequency_millis: 1000,
        },
    )
    .unwrap();
    let rows: Vec<_> = predictions
        .iter()
        .zip(labels)
        .enumerate()
        .map(|(i, (_, label))| {
            let time = chrono::DateTime::from_timestamp(i as i64, 0).unwrap();
            ResearchRow {
                series_id: 1,
                available_time: time,
                label_available_time: time + chrono::TimeDelta::seconds(5),
                signal: 0.0,
                label: *label / BPS,
                fee_bps: 2.0,
                latency_bps: 0.5,
                funding_bps: 0.0,
                pit_funding: false,
                features: BTreeMap::from([("spread_bps".into(), 0.0)]),
            }
        })
        .collect();
    let ledger = rows
        .iter()
        .zip(predictions)
        .enumerate()
        .map(|(i, (r, p))| PositionEvaluationPoint {
            quoted_turnover_fraction: Some(0.0),
            entry_target: Some(0.0),
            row_index: i,
            series_id: r.series_id,
            available_time: r.available_time,
            fold_index: 1,
            prediction: *p / BPS,
            target_position: 0.0,
            gross_return: 0.0,
            transaction_cost: 0.0,
            funding_cost: 0.0,
            net_return: 0.0,
            equity: 1.0,
        })
        .collect();
    (rows, ledger, protocol, policy)
}

#[test]
fn prediction_diagnostics_use_each_rows_cost_and_exclude_tail_signals() {
    let (mut rows, ledger, protocol, policy) = fixture(
        &[1.0, 5.0, 6.0, 6.0, -6.0, 1.0, 9.0, 9.0, 9.0, 9.0, 9.0],
        &[6.0, 6.0, 7.0, 7.0, -7.0, 0.0, 12.0, 12.0, 12.0, 12.0, 12.0],
    );
    rows[3].features.insert("spread_bps".into(), 2.0); // 7 bp here, 5 bp elsewhere.
    let (counts, stats, buckets) = summarize(&rows, &ledger, &protocol, &policy).unwrap();
    assert_eq!(counts.horizon_eligible_rows, 6);
    assert_eq!(counts.tail_rows, 5);
    assert_eq!(counts.eligible_predictions_above_cost, 2); // equality at 5 bp does not enter.
    assert_eq!(counts.eligible_labels_above_cost, 4); // equality at the 7 bp row is excluded.
    assert_eq!(counts.eligible_nonzero_entry_signals, 2);
    assert_eq!(counts.nonzero_position_rows, 0); // observations/signals are not filled trades.
    assert_eq!(buckets.iter().map(|b| b.observations).sum::<usize>(), 6);
    assert_eq!(buckets[3].observations, 3); // ratio [1,2) includes threshold equality descriptively.
    assert_eq!(buckets[3].directionally_correct_labels_above_cost, 3);
    assert!(stats.mae_bps > 0.0 && stats.mse_over_zero_prediction.unwrap() < 1.0);
    let mut long = policy;
    long.long_only = true;
    let (counts, _, _) = summarize(&rows, &ledger, &protocol, &long).unwrap();
    assert_eq!(counts.eligible_predictions_above_cost, 2);
    assert_eq!(counts.eligible_nonzero_entry_signals, 1);
    assert_eq!(counts.eligible_signals_blocked_by_direction, 1);
}

#[test]
fn prediction_diagnostics_report_calibration_in_basis_points_without_recalibrating() {
    let preds = [-3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0];
    let labels: Vec<_> = preds.iter().map(|p| 2.0 * p).collect();
    let (rows, ledger, protocol, policy) = fixture(&preds, &labels);
    let original = ledger.clone();
    let (_, stats, _) = summarize(&rows, &ledger, &protocol, &policy).unwrap();
    assert!((stats.calibration_slope.unwrap() - 2.0).abs() < 1e-12);
    assert!((stats.mse_over_zero_prediction.unwrap() - 0.25).abs() < 1e-12);
    assert!((stats.mae_bps - 2.0).abs() < 1e-12);
    assert_eq!(ledger, original);
}

#[test]
fn prediction_diagnostics_handle_zero_variance_and_reject_wrong_rows() {
    let (mut rows, mut ledger, protocol, policy) = fixture(&[0.0; 8], &[0.0; 8]);
    let (_, stats, _) = summarize(&rows, &ledger, &protocol, &policy).unwrap();
    assert_eq!(stats.calibration_slope, None);
    assert_eq!(stats.mse_over_zero_prediction, None);
    rows[0].fee_bps = 0.0;
    rows[0].latency_bps = 0.0;
    assert_eq!(
        summarize(&rows, &ledger, &protocol, &policy)
            .unwrap()
            .0
            .zero_cost_rows,
        1
    );
    ledger[0].row_index = 1;
    assert!(summarize(&rows, &ledger, &protocol, &policy)
        .unwrap_err()
        .contains("identity"));
    ledger[0].row_index = 0;
    rows[0].label = f64::NAN;
    assert!(summarize(&rows, &ledger, &protocol, &policy).is_err());
}
