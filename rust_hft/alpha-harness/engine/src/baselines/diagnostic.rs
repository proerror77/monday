//! Offline technical diagnostics. This module grants no compute or deployment authority.
use super::{
    classic::{fit_ridge, predict_ridge},
    horizon_target_positions, CexSupervisedDecisionPolicyV2,
};
use crate::{
    evaluation::{prepare_dataset, EngineContext, ResearchRow, WalkForwardFold},
    formula_evaluator::{FormulaEvaluator, PositionEvaluationReport},
};
use alpha_domain::{CexBaselineModelV1, EvaluationProtocolV1, FormulaEvaluatorConfig};
use serde::{Deserialize, Serialize};
use std::ops::Range;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticBaselineSpec {
    pub protocol: EvaluationProtocolV1,
    pub features: Vec<String>,
    pub ridge_l2: f64,
    pub evaluator: FormulaEvaluatorConfig,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionMetrics {
    pub rows: usize,
    pub mean_squared_error: f64,
    pub mean_absolute_error: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticFold {
    pub partition: WalkForwardFold,
    pub model: CexBaselineModelV1,
    pub training: RegressionMetrics,
    pub out_of_sample: RegressionMetrics,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticBaselineReport {
    pub schema: String,
    pub diagnostic_only: bool,
    pub deployment_authority: bool,
    pub statistical_strategy_acceptance: bool,
    pub input_rows: usize,
    pub fit_count: usize,
    pub protocol: EvaluationProtocolV1,
    pub feature_order: Vec<String>,
    pub folds: Vec<DiagnosticFold>,
    pub out_of_sample_accounting: PositionEvaluationReport,
}
fn regression(rows: &[ResearchRow], p: &[f64], r: Range<usize>) -> RegressionMetrics {
    let n = r.len() as f64;
    RegressionMetrics {
        rows: r.len(),
        mean_squared_error: r
            .clone()
            .map(|i| (p[i] - rows[i].label).powi(2))
            .sum::<f64>()
            / n,
        mean_absolute_error: r.map(|i| (p[i] - rows[i].label).abs()).sum::<f64>() / n,
    }
}
fn account(
    c: &EngineContext<'_>,
    p: &[f64],
    ranges: Vec<Range<usize>>,
    s: &DiagnosticBaselineSpec,
) -> Result<PositionEvaluationReport, String> {
    let h = s
        .protocol
        .labels
        .observation_frequency_millis
        .checked_mul(s.protocol.labels.horizon_buckets as u64)
        .ok_or("horizon overflow")?;
    let policy = CexSupervisedDecisionPolicyV2::hold_to_horizon_v3(h)?;
    let holding = policy.holding.as_ref().ok_or("missing holding policy")?;
    let targets =
        horizon_target_positions(c.rows(), p, &policy, c.protocol(), ranges.clone(), holding)?;
    FormulaEvaluator::new(s.evaluator.clone())?
        .with_decision_policy(&policy)?
        .evaluate_predictions_and_positions(
            c.rows(),
            p,
            &targets,
            ranges,
            alpha_domain::CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
            c.protocol(),
        )
}
pub fn run_diagnostic_baseline(
    rows: Vec<ResearchRow>,
    s: &DiagnosticBaselineSpec,
) -> Result<DiagnosticBaselineReport, String> {
    if s.features.is_empty()
        || s.features.len() > 8
        || !s.ridge_l2.is_finite()
        || s.ridge_l2 <= 0.0
        || s.protocol.walk_forward.fold_count == 0
        || s.protocol.walk_forward.fold_count > 3
        || !s.protocol.costs.cross_spread
    {
        return Err("diagnostic requires 1–8 features, positive Ridge regularization, 1–3 folds and spread costs".into());
    }
    let mut names = s.features.clone();
    names.sort();
    names.dedup();
    if names.len() != s.features.len()
        || names
            .iter()
            .any(|n| matches!(n.as_str(), "label" | "target" | "future_return"))
    {
        return Err("duplicated feature or target used as feature".into());
    }
    let frequency = i64::try_from(s.protocol.labels.observation_frequency_millis)
        .map_err(|_| "invalid frequency")?;
    if rows.iter().any(|r| r.series_id != 1)
        || rows.windows(2).any(|p| {
            p[1].available_time
                .signed_duration_since(p[0].available_time)
                .num_milliseconds()
                != frequency
        })
    {
        return Err("diagnostic requires one continuous instrument; gaps cannot be bridged".into());
    }
    let input_rows = rows.len();
    let prepared = prepare_dataset(rows, &s.protocol).map_err(|e| e.to_string())?;
    let c = prepared.engine_context();
    let x = c
        .rows()
        .iter()
        .map(|r| {
            s.features
                .iter()
                .map(|n| {
                    r.features
                        .get(n)
                        .copied()
                        .ok_or_else(|| format!("missing feature {n}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let y = c.rows().iter().map(|r| r.label).collect::<Vec<_>>();
    let mut folds = Vec::new();
    let mut oos_predictions = vec![0.0; c.rows().len()];
    for fold in c.folds() {
        let fit = fit_ridge(&x, &y, fold.train.clone(), s.ridge_l2)?;
        let mut p = vec![0.0; c.rows().len()];
        for i in fold.train.clone().chain(fold.validation.clone()) {
            p[i] = predict_ridge(&fit, &x[i])?;
        }
        let training = regression(c.rows(), &p, fold.train.clone());
        let out_of_sample = regression(c.rows(), &p, fold.validation.clone());
        for index in fold.validation.clone() {
            oos_predictions[index] = p[index];
        }
        folds.push(DiagnosticFold {
            partition: fold.clone(),
            model: CexBaselineModelV1::Ridge {
                intercept: fit.intercept,
                means: fit.means,
                scales: fit.scales,
                coefficients: fit.coefficients,
            },
            training,
            out_of_sample,
        });
    }
    let out_of_sample_accounting = account(
        &c,
        &oos_predictions,
        c.folds()
            .iter()
            .map(|fold| fold.validation.clone())
            .collect(),
        s,
    )?;
    Ok(DiagnosticBaselineReport {
        schema: "monday.offline_ridge_diagnostic.v1".into(),
        diagnostic_only: true,
        deployment_authority: false,
        statistical_strategy_acceptance: false,
        input_rows,
        fit_count: folds.len(),
        protocol: s.protocol.clone(),
        feature_order: s.features.clone(),
        folds,
        out_of_sample_accounting,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::{TimeDelta, TimeZone, Utc};
    use std::collections::BTreeMap;
    fn fixture() -> (Vec<ResearchRow>, DiagnosticBaselineSpec) {
        let start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let mids = (0..305)
            .map(|i| 100.0 + 0.01 * i as f64 + 0.03 * (i as f64 / 7.0).sin())
            .collect::<Vec<_>>();
        let rows = (0..300)
            .map(|i| ResearchRow {
                series_id: 1,
                available_time: start + TimeDelta::seconds(i as i64),
                label_available_time: start + TimeDelta::seconds(i as i64 + 5),
                signal: 0.0,
                features: BTreeMap::from([
                    ("book_imbalance".into(), (i as f64 / 7.0).cos()),
                    ("mid_price".into(), mids[i]),
                    ("spread_bps".into(), 1.0),
                ]),
                label: mids[i + 5] / mids[i] - 1.0,
                fee_bps: 2.0,
                funding_bps: 0.0,
                pit_funding: false,
                latency_bps: 0.5,
            })
            .collect();
        let protocol = EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 120,
                validation_rows: 30,
                fold_count: 3,
                purge_rows: 5,
                embargo_rows: 5,
                sealed_holdout_rows: 20,
            },
            EvaluationCostsV1 {
                fee_bps: 2.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 1.0,
                cross_spread: true,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1000,
            },
        )
        .unwrap();
        (
            rows,
            DiagnosticBaselineSpec {
                protocol,
                features: vec!["book_imbalance".into()],
                ridge_l2: 1.0,
                evaluator: FormulaEvaluatorConfig::default(),
            },
        )
    }
    #[test]
    fn diagnostic_baseline_historical_clock_runs_train_and_oos() {
        let (rows, s) = fixture();
        let r = run_diagnostic_baseline(rows, &s).unwrap();
        assert_eq!(r.fit_count, 3);
        assert_eq!(r.folds[0].training.rows, 120);
        assert_eq!(r.folds[0].out_of_sample.rows, 30);
        assert!(!r.deployment_authority);
        assert!(!r.statistical_strategy_acceptance);
        assert!(!r.out_of_sample_accounting.ledger.is_empty());
        r.out_of_sample_accounting.evaluation.validate().unwrap();
    }
    #[test]
    fn diagnostic_baseline_validation_labels_cannot_change_first_model() {
        let (rows, s) = fixture();
        let a = run_diagnostic_baseline(rows.clone(), &s).unwrap();
        let mut changed = rows;
        for row in &mut changed[125..155] {
            row.label += 0.123;
        }
        let b = run_diagnostic_baseline(changed, &s).unwrap();
        assert_eq!(a.folds[0].model, b.folds[0].model);
        assert_eq!(a.folds[0].training, b.folds[0].training);
        assert_ne!(a.folds[0].out_of_sample, b.folds[0].out_of_sample);
    }
    #[test]
    fn diagnostic_baseline_gap_and_immature_labels_fail_closed() {
        let (mut rows, s) = fixture();
        rows.remove(50);
        assert!(run_diagnostic_baseline(rows, &s)
            .unwrap_err()
            .contains("continuous"));
        let (mut rows, s) = fixture();
        rows[119].label_available_time = rows[130].available_time;
        assert!(run_diagnostic_baseline(rows, &s).is_err());
    }
    #[test]
    fn diagnostic_baseline_closed_holdout_and_costs_remain_effective() {
        let (mut rows, mut s) = fixture();
        for row in &mut rows {
            row.fee_bps = 0.1;
            row.latency_bps = 0.1;
        }
        s.protocol.costs.fee_bps = 0.1;
        s.protocol.costs.latency_bps = 0.1;
        s.protocol.costs.slippage_bps = 0.1;
        let a = run_diagnostic_baseline(rows.clone(), &s).unwrap();
        assert!(a
            .out_of_sample_accounting
            .ledger
            .iter()
            .any(|p| p.transaction_cost > 0.0));
        assert!(a
            .out_of_sample_accounting
            .ledger
            .iter()
            .all(
                |p| (p.net_return - (p.gross_return - p.transaction_cost - p.funding_cost)).abs()
                    < 1e-12
            ));
        for row in &mut rows[280..] {
            row.label = -0.9;
        }
        let b = run_diagnostic_baseline(rows.clone(), &s).unwrap();
        assert_eq!(a.out_of_sample_accounting, b.out_of_sample_accounting);
        for (first, changed) in a.folds.iter().zip(&b.folds) {
            assert_eq!(first.model, changed.model);
            assert_eq!(first.training, changed.training);
        }
        s.protocol.costs.slippage_bps = 100.0;
        let expensive = run_diagnostic_baseline(rows, &s).unwrap();
        assert!(expensive
            .out_of_sample_accounting
            .ledger
            .iter()
            .all(|p| p.target_position == 0.0));
    }
}
