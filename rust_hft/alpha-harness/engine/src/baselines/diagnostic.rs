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

/// A predeclared two-hypothesis technical study. It cannot complete a Campaign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCycleReport {
    pub schema: String,
    pub lineage_digest_encoding: String,
    pub diagnostic_only: bool,
    pub deployment_authority: bool,
    pub statistical_strategy_acceptance: bool,
    pub fit_limit: usize,
    pub fit_count: usize,
    pub automatic_iterations: usize,
    pub baseline_spec: DiagnosticBaselineSpec,
    pub baseline: DiagnosticBaselineReport,
    pub reproducibility: DiagnosticRepeat,
    pub successor: Option<DiagnosticSuccessor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRepeat {
    pub report_sha256: String,
    pub repeated_report_sha256: String,
    pub byte_identical: bool,
    pub fit_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticSuccessor {
    pub rule: String,
    pub parent_report_sha256: String,
    pub parent_score: f64,
    pub parent_gate_reasons: Vec<String>,
    pub feedback_available_until: chrono::DateTime<chrono::Utc>,
    pub first_prediction_at: chrono::DateTime<chrono::Utc>,
    pub spec: DiagnosticBaselineSpec,
    pub spec_sha256: String,
    pub report: DiagnosticBaselineReport,
}

/// No tuning or retries: two baseline folds, their identical repeat, and at most
/// two successor folds. Feedback sees only the native development/OOS report.
pub fn run_diagnostic_cycle(
    rows: Vec<ResearchRow>,
    spec: &DiagnosticBaselineSpec,
) -> Result<DiagnosticCycleReport, String> {
    if spec.features != ["book_imbalance"]
        || spec.protocol.walk_forward.fold_count != 2
        || spec.evaluator.multiple_testing_trials != 2
        || spec.protocol.calendar.is_some()
    {
        return Err(
            "cycle requires book_imbalance, two ordinary folds and two predeclared hypotheses"
                .into(),
        );
    }
    // Missing causal depth fails before consuming any fit; never fabricate it.
    if rows.iter().any(|row| {
        row.features
            .get("book_imbalance_top5")
            .is_none_or(|value| !value.is_finite())
    }) {
        return Err("cycle requires causally materialized book_imbalance_top5".into());
    }
    // Freeze the successor's later window from actual validated partitions,
    // before fitting. It must not score the data that selected its hypothesis.
    let prepared =
        prepare_dataset(rows.clone(), &spec.protocol).map_err(|error| error.to_string())?;
    let context = prepared.engine_context();
    let feedback_available_until = context
        .folds()
        .iter()
        .flat_map(|fold| fold.validation.clone())
        .map(|index| context.rows()[index].label_available_time)
        .max()
        .ok_or("baseline has no feedback window")?;
    let mut next_spec = spec.clone();
    next_spec.features = vec!["book_imbalance_top5".into()];
    next_spec.protocol.walk_forward.initial_train_rows = context
        .folds()
        .last()
        .ok_or("baseline has no final partition")?
        .embargo
        .end;
    let next_prepared = prepare_dataset(rows.clone(), &next_spec.protocol)
        .map_err(|error| format!("successor window is invalid before any fit: {error}"))?;
    let next_context = next_prepared.engine_context();
    let first_prediction_at =
        next_context.rows()[next_context.folds()[0].validation.start].available_time;
    let embargo_millis = spec
        .protocol
        .labels
        .observation_frequency_millis
        .checked_mul(
            u64::try_from(spec.protocol.walk_forward.embargo_rows)
                .map_err(|_| "embargo overflow")?,
        )
        .and_then(|value| i64::try_from(value).ok())
        .ok_or("feedback embargo overflow")?;
    let feedback_deadline = feedback_available_until
        .checked_add_signed(chrono::TimeDelta::milliseconds(embargo_millis))
        .ok_or("feedback embargo clock overflow")?;
    if first_prediction_at <= feedback_deadline {
        return Err(
            "successor predictions must follow parent label availability and embargo".into(),
        );
    }
    let digest = |value: &DiagnosticBaselineReport| {
        alpha_domain::canonical_json_hash(value).map_err(|error| error.to_string())
    };
    let baseline = run_diagnostic_baseline(rows.clone(), spec)?;
    let repeated = run_diagnostic_baseline(rows.clone(), spec)?;
    let first_bytes = serde_json::to_vec(&baseline).map_err(|error| error.to_string())?;
    let repeated_bytes = serde_json::to_vec(&repeated).map_err(|error| error.to_string())?;
    if first_bytes != repeated_bytes {
        return Err("baseline repeat changed model/report bytes; successor is blocked".into());
    }
    let parent_sha256 = digest(&baseline)?;
    let reproducibility = DiagnosticRepeat {
        report_sha256: parent_sha256.clone(),
        repeated_report_sha256: digest(&repeated)?,
        byte_identical: true,
        fit_count: repeated.fit_count,
    };
    let mut fit_count = baseline.fit_count + repeated.fit_count;
    let feedback = &baseline.out_of_sample_accounting.evaluation;
    let successor = if feedback.passed {
        None
    } else {
        let spec_sha256 =
            alpha_domain::canonical_json_hash(&next_spec).map_err(|error| error.to_string())?;
        let report = run_diagnostic_baseline(rows, &next_spec)?;
        fit_count += report.fit_count;
        Some(DiagnosticSuccessor {
            rule: "later_top_five_after_native_development_gate_rejection_v1".into(),
            parent_report_sha256: parent_sha256,
            parent_score: feedback.score,
            parent_gate_reasons: feedback.failure_reasons.clone(),
            feedback_available_until,
            first_prediction_at,
            spec: next_spec,
            spec_sha256,
            report,
        })
    };
    Ok(DiagnosticCycleReport {
        schema: "monday.offline_ridge_diagnostic_cycle.v1".into(),
        lineage_digest_encoding: "monday.canonical_json_sha256".into(),
        diagnostic_only: true,
        deployment_authority: false,
        statistical_strategy_acceptance: false,
        fit_limit: 6,
        fit_count,
        automatic_iterations: usize::from(successor.is_some()),
        baseline_spec: spec.clone(),
        baseline,
        reproducibility,
        successor,
    })
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

    fn cycle_fixture() -> (Vec<ResearchRow>, DiagnosticBaselineSpec) {
        let (mut rows, mut spec) = fixture();
        spec.protocol.walk_forward.fold_count = 2;
        spec.evaluator.multiple_testing_trials = 2;
        for (index, row) in rows.iter_mut().enumerate() {
            row.features
                .insert("book_imbalance_top5".into(), (index as f64 / 11.0).sin());
        }
        (rows, spec)
    }

    #[test]
    fn diagnostic_cycle_binds_feedback_and_executes_only_the_predeclared_successor() {
        let (rows, spec) = cycle_fixture();
        let report = run_diagnostic_cycle(rows.clone(), &spec).unwrap();
        assert_eq!(report.fit_count, 6);
        assert_eq!(report.automatic_iterations, 1);
        assert!(report.reproducibility.byte_identical);
        assert_eq!(report.reproducibility.fit_count, 2);
        let child = report.successor.as_ref().unwrap();
        assert_eq!(child.report.fit_count, 2);
        assert_eq!(child.spec.features, ["book_imbalance_top5"]);
        let mut expected = spec.clone();
        expected.features = vec!["book_imbalance_top5".into()];
        expected.protocol.walk_forward.initial_train_rows =
            report.baseline.folds.last().unwrap().partition.embargo.end;
        assert_eq!(
            serde_json::to_vec(&child.spec).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let feedback = &report.baseline.out_of_sample_accounting.evaluation;
        assert!(!feedback.passed);
        assert_eq!(child.parent_score, feedback.score);
        assert_eq!(child.parent_gate_reasons, feedback.failure_reasons);
        let observed_feedback_time = report
            .baseline
            .folds
            .iter()
            .flat_map(|fold| fold.partition.validation.clone())
            .map(|index| rows[index].label_available_time)
            .max()
            .unwrap();
        assert_eq!(child.feedback_available_until, observed_feedback_time);
        assert!(child.first_prediction_at > child.feedback_available_until + TimeDelta::seconds(5));
        assert_eq!(
            child.first_prediction_at,
            rows[child.report.folds[0].partition.validation.start].available_time
        );
        assert_eq!(
            child.parent_report_sha256,
            alpha_domain::canonical_json_hash(&report.baseline).unwrap()
        );
        assert_eq!(
            child.spec_sha256,
            alpha_domain::canonical_json_hash(&child.spec).unwrap()
        );
        let expected_child = run_diagnostic_baseline(rows, &child.spec).unwrap();
        assert_eq!(
            serde_json::to_vec(&child.report).unwrap(),
            serde_json::to_vec(&expected_child).unwrap()
        );
        assert!(!report.deployment_authority);
        assert!(!report.statistical_strategy_acceptance);
    }

    #[test]
    fn diagnostic_cycle_sealed_tail_cannot_change_feedback_models_or_successor() {
        let (rows, spec) = cycle_fixture();
        let first = run_diagnostic_cycle(rows.clone(), &spec).unwrap();
        let mut changed = rows;
        for row in &mut changed[280..] {
            row.label = -0.9;
            row.features.insert("book_imbalance".into(), -0.99);
            row.features.insert("book_imbalance_top5".into(), 0.99);
        }
        let second = run_diagnostic_cycle(changed, &spec).unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn diagnostic_cycle_rejects_unplanned_trials_features_depth_and_gaps() {
        let (rows, spec) = cycle_fixture();
        let mut wrong = spec.clone();
        wrong.protocol.walk_forward.fold_count = 3;
        assert!(run_diagnostic_cycle(rows.clone(), &wrong).is_err());
        wrong = spec.clone();
        wrong.evaluator.multiple_testing_trials = 1;
        assert!(run_diagnostic_cycle(rows.clone(), &wrong).is_err());
        wrong = spec.clone();
        wrong.features.push("mid_price".into());
        assert!(run_diagnostic_cycle(rows.clone(), &wrong).is_err());
        let mut missing = rows.clone();
        missing[0].features.remove("book_imbalance_top5");
        assert!(run_diagnostic_cycle(missing, &spec).is_err());
        let mut gap = rows;
        gap.remove(50);
        assert!(run_diagnostic_cycle(gap, &spec).is_err());
    }

    #[test]
    fn diagnostic_cycle_future_successor_labels_cannot_change_parent_feedback_or_plan() {
        let (rows, spec) = cycle_fixture();
        let first = run_diagnostic_cycle(rows.clone(), &spec).unwrap();
        let child = first.successor.as_ref().unwrap();
        let mut changed = rows;
        for fold in &child.report.folds {
            for row in &mut changed[fold.partition.validation.clone()] {
                row.label += 0.1;
            }
        }
        let second = run_diagnostic_cycle(changed, &spec).unwrap();
        assert_eq!(
            alpha_domain::canonical_json_hash(&first.baseline).unwrap(),
            alpha_domain::canonical_json_hash(&second.baseline).unwrap()
        );
        let changed_child = second.successor.as_ref().unwrap();
        assert_eq!(
            child.parent_report_sha256,
            changed_child.parent_report_sha256
        );
        assert_eq!(child.spec_sha256, changed_child.spec_sha256);
        assert_eq!(
            child.report.folds[0].model,
            changed_child.report.folds[0].model
        );
        assert_ne!(
            child.report.folds[0].out_of_sample,
            changed_child.report.folds[0].out_of_sample
        );
    }

    #[test]
    fn diagnostic_cycle_requires_a_later_window_before_consuming_fits() {
        let (mut rows, spec) = cycle_fixture();
        rows.truncate(230);
        assert!(run_diagnostic_baseline(rows.clone(), &spec).is_ok());
        assert!(run_diagnostic_cycle(rows, &spec)
            .unwrap_err()
            .contains("successor window is invalid before any fit"));
    }
}
