use crate::{
    evaluation::{EngineContext, WalkForwardFold},
    formula_evaluator::{evaluate_ast, FormulaEvaluator},
};
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineCartNodeV1, CexBaselineFoldV1,
    CexBaselineGateV1, CexBaselineModelKindV1, CexBaselineModelV1, CexBaselinePolicyV1,
    CexBaselineRangeV1, CexFactorBankRevisionV2, CexFactorOrientationV1, CexResearchContentRefV1,
    CexResearchHypothesisTargetV1, CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
};
use serde_json::json;

use crate::engines::solve;

#[derive(Debug, Clone, PartialEq)]
pub struct CexBaselineRun {
    pub ridge: Option<CexBaselineArtifactV1>,
    pub cart: Option<CexBaselineArtifactV1>,
    pub gate: CexBaselineGateV1,
}

pub fn verify_cex_baseline_artifact(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    artifact: &CexBaselineArtifactV1,
) -> Result<(), String> {
    artifact
        .validate()
        .map_err(|error| format!("baseline artifact validation failed: {error}"))?;
    factor_bank
        .validate()
        .map_err(|error| format!("factor bank validation failed: {error}"))?;
    if artifact.factor_bank_revision_id != factor_bank.revision_id
        || artifact.research_dataset != factor_bank.research_dataset
        || artifact.evaluation_policy != factor_bank.evaluation_policy
        || artifact.walk_forward_partition != factor_bank.walk_forward_partition
    {
        return Err("baseline artifact Factor Bank binding drifted".to_string());
    }
    validate_context_identity(
        context,
        factor_bank,
        &artifact.evaluation_policy,
        &artifact.target,
    )?;
    let (factor_ids, factors) = evaluate_factor_features(context, factor_bank)?;
    if artifact.factor_ids != factor_ids {
        return Err("baseline artifact factor ordering drifted".to_string());
    }
    let features = transpose_factors(&factors, context.rows().len())?;
    if artifact.folds.len() != context.folds().len() {
        return Err("baseline artifact fold count drifted".to_string());
    }
    let labels = labels(context.rows());
    let mut signals = vec![0.0; context.rows().len()];
    let mut ranges = Vec::with_capacity(artifact.folds.len());
    for (fold_index, (fold, context_fold)) in artifact.folds.iter().zip(context.folds()).enumerate()
    {
        let validation = fold.validation_range.start..fold.validation_range.end;
        if fold.train_range.start != context_fold.train.start
            || fold.train_range.end != context_fold.train.end
            || fold.purge_range.start != context_fold.purge.start
            || fold.purge_range.end != context_fold.purge.end
            || fold.validation_range.start != context_fold.validation.start
            || fold.validation_range.end != context_fold.validation.end
            || fold.embargo_range.start != context_fold.embargo.start
            || fold.embargo_range.end != context_fold.embargo.end
            || validation.end > features.len()
            || fold.predictions.len() != validation.len()
        {
            return Err(format!(
                "baseline fold {} validation range drifted",
                fold_index + 1
            ));
        }
        let predictions = match &fold.model {
            CexBaselineModelV1::Ridge { .. } => {
                let fit = fit_ridge(
                    &features,
                    &labels,
                    context_fold.train.clone(),
                    artifact.baseline_policy.ridge_l2,
                )?;
                let refit_model = CexBaselineModelV1::Ridge {
                    intercept: fit.intercept,
                    means: fit.means.clone(),
                    scales: fit.scales.clone(),
                    coefficients: fit.coefficients.clone(),
                };
                if refit_model != fold.model {
                    return Err(format!(
                        "baseline fold {} Ridge model drifted",
                        fold_index + 1
                    ));
                }
                predict_fold_ridge(&fit, &features, &validation)?
            }
            CexBaselineModelV1::ShallowCart { .. } => {
                let tree = fit_cart(
                    &features,
                    &labels,
                    context_fold.train.clone(),
                    artifact.baseline_policy.cart_max_depth,
                    artifact.baseline_policy.cart_min_leaf,
                    &factor_ids,
                )?;
                let refit_model = CexBaselineModelV1::ShallowCart {
                    root: cart_node(tree.clone()),
                };
                if refit_model != fold.model {
                    return Err(format!(
                        "baseline fold {} CART model drifted",
                        fold_index + 1
                    ));
                }
                predict_fold_cart(&tree, &features, &validation)?
            }
        };
        if !predictions_equal(&predictions, &fold.predictions) {
            return Err(format!(
                "baseline fold {} predictions drifted",
                fold_index + 1
            ));
        }
        for (index, prediction) in validation.clone().zip(predictions) {
            signals[index] = prediction;
        }
        ranges.push(validation);
    }
    let evaluator = FormulaEvaluator::new(artifact.baseline_policy.evaluator_config.clone())?;
    let evaluation = evaluator.evaluate_signals(
        context.rows(),
        &signals,
        ranges,
        CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )?;
    if evaluation != artifact.evaluation {
        return Err("baseline evaluation drifted".to_string());
    }
    Ok(())
}

pub fn evaluate_cex_baselines(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    policy: &CexBaselinePolicyV1,
    mission_id: &str,
    target: CexResearchHypothesisTargetV1,
    evaluation_policy: &CexResearchContentRefV1,
) -> Result<CexBaselineRun, String> {
    if mission_id.trim().is_empty() {
        return Err("baseline mission identity is empty".to_string());
    }
    factor_bank
        .validate()
        .map_err(|error| format!("factor bank validation failed: {error}"))?;
    policy
        .validate()
        .map_err(|error| format!("baseline policy validation failed: {error}"))?;
    evaluation_policy
        .validate()
        .map_err(|error| format!("evaluation policy validation failed: {error}"))?;
    if factor_bank.evaluation_policy != *evaluation_policy {
        return Err("baseline evaluation policy does not match the Factor Bank".to_string());
    }
    validate_context_identity(context, factor_bank, evaluation_policy, &target)?;
    if factor_bank.entries.is_empty() {
        let gate = CexBaselineGateV1::empty_factor_bank(mission_id, policy, factor_bank)
            .map_err(|error| format!("empty Factor Bank gate failed: {error}"))?;
        return Ok(CexBaselineRun {
            ridge: None,
            cart: None,
            gate,
        });
    }
    let (factor_ids, factors) = evaluate_factor_features(context, factor_bank)?;
    let feature_rows = transpose_factors(&factors, context.rows().len())?;
    let ridge = fit_artifact(
        context,
        policy,
        mission_id,
        target.clone(),
        evaluation_policy,
        factor_bank,
        factor_ids.clone(),
        &feature_rows,
        BaselineKind::Ridge,
    )?;
    let cart = fit_artifact(
        context,
        policy,
        mission_id,
        target,
        evaluation_policy,
        factor_bank,
        factor_ids,
        &feature_rows,
        BaselineKind::ShallowCart,
    )?;
    let gate = CexBaselineGateV1::new(&ridge, &cart)
        .map_err(|error| format!("baseline gate failed: {error}"))?;
    Ok(CexBaselineRun {
        ridge: Some(ridge),
        cart: Some(cart),
        gate,
    })
}

#[derive(Debug, Clone, Copy)]
enum BaselineKind {
    Ridge,
    ShallowCart,
}

fn validate_context_identity(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    evaluation_policy: &CexResearchContentRefV1,
    target: &CexResearchHypothesisTargetV1,
) -> Result<(), String> {
    if target.name.trim().is_empty()
        || target.horizon != context.protocol().labels
        || factor_bank.research_dataset.validate().is_err()
        || factor_bank.walk_forward_partition.validate().is_err()
    {
        return Err("baseline target or Factor Bank identity is invalid".to_string());
    }
    let dataset_hash = canonical_json_hash(&context.rows())
        .map_err(|error| format!("research dataset identity failed: {error}"))?;
    if dataset_hash != factor_bank.research_dataset.content_sha256 {
        return Err("baseline research dataset identity drifted".to_string());
    }
    let partition_hash = canonical_json_hash(&json!({
        "research_dataset": &factor_bank.research_dataset,
        "folds": context.folds(),
    }))
    .map_err(|error| format!("walk-forward partition identity failed: {error}"))?;
    if partition_hash != factor_bank.walk_forward_partition.content_sha256 {
        return Err("baseline walk-forward partition identity drifted".to_string());
    }
    if context
        .protocol()
        .content_hash()
        .map_err(|error| format!("evaluation protocol identity failed: {error}"))?
        != evaluation_policy.content_sha256
    {
        return Err("baseline evaluation protocol identity drifted".to_string());
    }
    for fold in context.folds() {
        validate_fold_range(fold, context.rows().len())?;
    }
    Ok(())
}

fn validate_fold_range(fold: &WalkForwardFold, row_count: usize) -> Result<(), String> {
    if fold.train.start >= fold.train.end
        || fold.embargo.start > fold.embargo.end
        || fold.purge.start > fold.purge.end
        || fold.validation.start >= fold.validation.end
        || fold.train.end > fold.purge.start
        || fold.purge.end > fold.validation.start
        || fold.validation.end > fold.embargo.start
        || fold.embargo.end > row_count
    {
        return Err("walk-forward fold range is invalid".to_string());
    }
    Ok(())
}

fn evaluate_factor_features(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
) -> Result<(Vec<String>, Vec<Vec<f64>>), String> {
    let mut entries = factor_bank.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.factor_id.cmp(&right.factor_id));
    let factor_ids = entries
        .iter()
        .map(|entry| entry.factor_id.clone())
        .collect::<Vec<_>>();
    let factors = evaluate_factor_features_from_entries(context, &entries)?;
    Ok((factor_ids, factors))
}

fn evaluate_factor_features_from_entries(
    context: &EngineContext<'_>,
    entries: &[&alpha_domain::CexFactorBankEntryV1],
) -> Result<Vec<Vec<f64>>, String> {
    entries
        .iter()
        .map(|entry| {
            let mut values = evaluate_ast(&entry.canonical_ast, context.rows())?;
            if values.iter().any(|value| !value.is_finite()) {
                return Err(format!(
                    "factor {} produced a non-finite value",
                    entry.factor_id
                ));
            }
            if entry.orientation == CexFactorOrientationV1::Negative {
                for value in &mut values {
                    *value = normalize_zero(-*value);
                }
            }
            Ok(values)
        })
        .collect::<Result<Vec<Vec<f64>>, String>>()
}

fn transpose_factors(factors: &[Vec<f64>], row_count: usize) -> Result<Vec<Vec<f64>>, String> {
    if factors.is_empty() || factors.iter().any(|factor| factor.len() != row_count) {
        return Err("Factor Bank feature matrix is invalid".to_string());
    }
    let mut rows = vec![Vec::with_capacity(factors.len()); row_count];
    for factor in factors {
        for (row, value) in rows.iter_mut().zip(factor) {
            row.push(normalize_zero(*value));
        }
    }
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
fn fit_artifact(
    context: &EngineContext<'_>,
    policy: &CexBaselinePolicyV1,
    mission_id: &str,
    target: CexResearchHypothesisTargetV1,
    evaluation_policy: &CexResearchContentRefV1,
    factor_bank: &CexFactorBankRevisionV2,
    factor_ids: Vec<String>,
    features: &[Vec<f64>],
    kind: BaselineKind,
) -> Result<CexBaselineArtifactV1, String> {
    let evaluator = FormulaEvaluator::new(policy.evaluator_config.clone())?;
    let mut folds = Vec::with_capacity(context.folds().len());
    let mut signals = vec![0.0; context.rows().len()];
    for (fold_index, fold) in context.folds().iter().enumerate() {
        let (model, predictions) = match kind {
            BaselineKind::Ridge => {
                let labels = labels(context.rows());
                let fit = fit_ridge(features, &labels, fold.train.clone(), policy.ridge_l2)?;
                let predictions = predict_fold_ridge(&fit, features, &fold.validation)?;
                (
                    CexBaselineModelV1::Ridge {
                        intercept: fit.intercept,
                        means: fit.means,
                        scales: fit.scales,
                        coefficients: fit.coefficients,
                    },
                    predictions,
                )
            }
            BaselineKind::ShallowCart => {
                let labels = labels(context.rows());
                let tree = fit_cart(
                    features,
                    &labels,
                    fold.train.clone(),
                    policy.cart_max_depth,
                    policy.cart_min_leaf,
                    &factor_ids,
                )?;
                let predictions = predict_fold_cart(&tree, features, &fold.validation)?;
                (
                    CexBaselineModelV1::ShallowCart {
                        root: cart_node(tree),
                    },
                    predictions,
                )
            }
        };
        for (index, prediction) in fold.validation.clone().zip(&predictions) {
            signals[index] = *prediction;
        }
        folds.push(
            CexBaselineFoldV1::new(
                fold_index + 1,
                range(&fold.train),
                range(&fold.purge),
                range(&fold.validation),
                range(&fold.embargo),
                predictions,
                model,
            )
            .map_err(|error| format!("baseline fold validation failed: {error}"))?,
        );
    }
    let evaluation = evaluator.evaluate_signals(
        context.rows(),
        &signals,
        context.folds().iter().map(|fold| fold.validation.clone()),
        CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )?;
    let model_kind = match kind {
        BaselineKind::Ridge => CexBaselineModelKindV1::Ridge,
        BaselineKind::ShallowCart => CexBaselineModelKindV1::ShallowCart,
    };
    let artifact = CexBaselineArtifactV1::new(
        mission_id.to_string(),
        factor_bank.revision_id.clone(),
        factor_ids,
        target,
        factor_bank.research_dataset.clone(),
        factor_bank.walk_forward_partition.clone(),
        evaluation_policy.clone(),
        policy.clone(),
        model_kind,
        folds,
        evaluation,
    )
    .map_err(|error| format!("baseline artifact validation failed: {error}"))?;
    verify_cex_baseline_artifact(context, factor_bank, &artifact)?;
    Ok(artifact)
}

fn labels(rows: &[crate::evaluation::ResearchRow]) -> Vec<f64> {
    rows.iter().map(|row| row.label).collect()
}

fn range(range: &std::ops::Range<usize>) -> CexBaselineRangeV1 {
    CexBaselineRangeV1 {
        start: range.start,
        end: range.end,
    }
}

fn predict_fold_ridge(
    model: &RidgeFit,
    features: &[Vec<f64>],
    range: &std::ops::Range<usize>,
) -> Result<Vec<f64>, String> {
    range
        .clone()
        .map(|index| predict_ridge(model, &features[index]))
        .collect()
}

fn predict_fold_cart(
    model: &CartNode,
    features: &[Vec<f64>],
    range: &std::ops::Range<usize>,
) -> Result<Vec<f64>, String> {
    range
        .clone()
        .map(|index| predict_cart(model, &features[index]))
        .collect()
}

fn predictions_equal(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

fn cart_node(node: CartNode) -> CexBaselineCartNodeV1 {
    match node {
        CartNode::Leaf {
            value,
            sample_count,
        } => CexBaselineCartNodeV1::Leaf {
            value,
            sample_count,
        },
        CartNode::Split {
            feature_index,
            threshold,
            left,
            right,
        } => CexBaselineCartNodeV1::Split {
            feature_index,
            threshold,
            left: Box::new(cart_node(*left)),
            right: Box::new(cart_node(*right)),
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RidgeFit {
    pub(crate) intercept: f64,
    pub(crate) means: Vec<f64>,
    pub(crate) scales: Vec<f64>,
    pub(crate) coefficients: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CartNode {
    Leaf {
        value: f64,
        sample_count: usize,
    },
    Split {
        feature_index: usize,
        threshold: f64,
        left: Box<CartNode>,
        right: Box<CartNode>,
    },
}

pub(crate) fn fit_ridge(
    features: &[Vec<f64>],
    labels: &[f64],
    rows: std::ops::Range<usize>,
    lambda: f64,
) -> Result<RidgeFit, String> {
    if !lambda.is_finite() || lambda <= 0.0 || labels.len() != features.len() {
        return Err("invalid Ridge inputs".to_string());
    }
    let width = features.first().map_or(0, Vec::len);
    if width == 0
        || features.iter().any(|row| row.len() != width)
        || rows.start >= rows.end
        || rows.end > features.len()
        || rows.end > labels.len()
        || features
            .iter()
            .flat_map(|row| row.iter())
            .chain(labels.iter())
            .any(|value| !value.is_finite())
    {
        return Err("invalid Ridge inputs".to_string());
    }

    let count = rows.end - rows.start;
    let mut means = vec![0.0; width];
    for row in &features[rows.clone()] {
        for (mean, value) in means.iter_mut().zip(row) {
            *mean += *value;
        }
    }
    for mean in &mut means {
        *mean /= count as f64;
        if !mean.is_finite() {
            return Err("Ridge mean is non-finite".to_string());
        }
    }
    let mut scales = vec![0.0; width];
    for row in &features[rows.clone()] {
        for ((scale, mean), value) in scales.iter_mut().zip(&means).zip(row) {
            *scale += (*value - *mean).powi(2);
        }
    }
    for scale in &mut scales {
        *scale = (*scale / count as f64).sqrt();
        if !scale.is_finite() {
            return Err("Ridge scale is non-finite".to_string());
        }
        if *scale <= f64::EPSILON {
            *scale = 1.0;
        }
    }

    let target_mean = labels[rows.clone()].iter().sum::<f64>() / count as f64;
    if !target_mean.is_finite() {
        return Err("Ridge target mean is non-finite".to_string());
    }
    let mut matrix = vec![vec![0.0; width]; width];
    let mut values = vec![0.0; width];
    for index in rows {
        let standardized = features[index]
            .iter()
            .zip(&means)
            .zip(&scales)
            .map(|((value, mean), scale)| (*value - *mean) / *scale)
            .collect::<Vec<_>>();
        let centered_target = labels[index] - target_mean;
        for feature in 0..width {
            values[feature] += standardized[feature] * centered_target;
            for other in 0..width {
                matrix[feature][other] += standardized[feature] * standardized[other];
            }
        }
    }
    for (index, row) in matrix.iter_mut().enumerate() {
        row[index] += lambda;
    }
    let coefficients = solve(matrix, values)?;
    if coefficients.iter().any(|value| !value.is_finite()) {
        return Err("Ridge coefficients are non-finite".to_string());
    }
    let intercept = target_mean;
    if !intercept.is_finite() {
        return Err("Ridge intercept is non-finite".to_string());
    }
    Ok(RidgeFit {
        intercept: normalize_zero(intercept),
        means: means.into_iter().map(normalize_zero).collect(),
        scales: scales.into_iter().map(normalize_zero).collect(),
        coefficients: coefficients.into_iter().map(normalize_zero).collect(),
    })
}

pub(crate) fn predict_ridge(model: &RidgeFit, features: &[f64]) -> Result<f64, String> {
    if features.len() != model.means.len()
        || model.means.len() != model.scales.len()
        || model.means.len() != model.coefficients.len()
        || features.iter().any(|value| !value.is_finite())
        || model
            .scales
            .iter()
            .any(|scale| !scale.is_finite() || *scale <= 0.0)
        || model
            .means
            .iter()
            .chain(&model.scales)
            .chain(&model.coefficients)
            .any(|value| !value.is_finite())
        || !model.intercept.is_finite()
    {
        return Err("invalid Ridge prediction inputs".to_string());
    }
    let prediction = model.intercept
        + features
            .iter()
            .zip(&model.means)
            .zip(&model.scales)
            .zip(&model.coefficients)
            .map(|(((value, mean), scale), coefficient)| coefficient * (value - mean) / scale)
            .sum::<f64>();
    prediction
        .is_finite()
        .then(|| normalize_zero(prediction))
        .ok_or_else(|| "Ridge prediction is non-finite".to_string())
}

pub(crate) fn fit_cart(
    features: &[Vec<f64>],
    labels: &[f64],
    rows: std::ops::Range<usize>,
    max_depth: usize,
    min_leaf: usize,
    factor_ids: &[String],
) -> Result<CartNode, String> {
    if labels.len() != features.len()
        || rows.start >= rows.end
        || rows.end > features.len()
        || rows.end > labels.len()
        || max_depth == 0
        || min_leaf == 0
    {
        return Err("invalid CART inputs".to_string());
    }
    let width = features[rows.start].len();
    if width == 0
        || factor_ids.len() != width
        || features[rows.clone()]
            .iter()
            .any(|row| row.len() != width || row.iter().any(|value| !value.is_finite()))
        || labels[rows.clone()].iter().any(|value| !value.is_finite())
    {
        return Err("invalid CART inputs".to_string());
    }
    let indices = rows.collect::<Vec<_>>();
    Ok(fit_cart_node(
        features, labels, &indices, 0, max_depth, min_leaf, factor_ids,
    ))
}

fn fit_cart_node(
    features: &[Vec<f64>],
    labels: &[f64],
    indices: &[usize],
    depth: usize,
    max_depth: usize,
    min_leaf: usize,
    factor_ids: &[String],
) -> CartNode {
    let value = indices.iter().map(|index| labels[*index]).sum::<f64>() / indices.len() as f64;
    let parent_loss = sse(labels, indices, value);
    if depth >= max_depth || indices.len() < min_leaf.saturating_mul(2) {
        return CartNode::Leaf {
            value: normalize_zero(value),
            sample_count: indices.len(),
        };
    }

    let mut best: Option<(f64, usize, f64)> = None;
    for feature_index in 0..factor_ids.len() {
        let mut sorted = indices.to_vec();
        sorted.sort_by(|left, right| {
            features[*left][feature_index]
                .total_cmp(&features[*right][feature_index])
                .then_with(|| left.cmp(right))
        });
        let total_sum = indices.iter().map(|index| labels[*index]).sum::<f64>();
        let total_sum_sq = indices
            .iter()
            .map(|index| labels[*index] * labels[*index])
            .sum::<f64>();
        let mut left_sum = 0.0;
        let mut left_sum_sq = 0.0;
        for split in 0..sorted.len() - 1 {
            let index = sorted[split];
            left_sum += labels[index];
            left_sum_sq += labels[index] * labels[index];
            let left_value = features[index][feature_index];
            let right_value = features[sorted[split + 1]][feature_index];
            if left_value == right_value {
                continue;
            }
            let threshold = left_value;
            let left_count = split + 1;
            let right_count = sorted.len() - left_count;
            if left_count < min_leaf || right_count < min_leaf {
                continue;
            }
            let right_sum = total_sum - left_sum;
            let right_sum_sq = total_sum_sq - left_sum_sq;
            let loss = sse_from_sums(left_sum, left_sum_sq, left_count)
                + sse_from_sums(right_sum, right_sum_sq, right_count);
            if !loss.is_finite() || loss >= parent_loss {
                continue;
            }
            let replace = best
                .as_ref()
                .is_none_or(|(best_loss, best_feature, best_threshold)| {
                    loss.total_cmp(best_loss)
                        .then_with(|| factor_ids[feature_index].cmp(&factor_ids[*best_feature]))
                        .then_with(|| threshold.total_cmp(best_threshold))
                        .is_lt()
                });
            if replace {
                best = Some((loss, feature_index, threshold));
            }
        }
    }
    let Some((_, feature_index, threshold)) = best else {
        return CartNode::Leaf {
            value: normalize_zero(value),
            sample_count: indices.len(),
        };
    };
    let (left, right): (Vec<_>, Vec<_>) = indices
        .iter()
        .copied()
        .partition(|index| features[*index][feature_index] <= threshold);
    CartNode::Split {
        feature_index,
        threshold: normalize_zero(threshold),
        left: Box::new(fit_cart_node(
            features,
            labels,
            &left,
            depth + 1,
            max_depth,
            min_leaf,
            factor_ids,
        )),
        right: Box::new(fit_cart_node(
            features,
            labels,
            &right,
            depth + 1,
            max_depth,
            min_leaf,
            factor_ids,
        )),
    }
}

fn sse(labels: &[f64], indices: &[usize], mean: f64) -> f64 {
    indices
        .iter()
        .map(|index| (labels[*index] - mean).powi(2))
        .sum()
}

fn sse_from_sums(sum: f64, sum_sq: f64, count: usize) -> f64 {
    let raw = sum_sq - (sum * sum / count as f64);
    let tolerance = f64::EPSILON * 64.0 * sum_sq.abs().max(1.0);
    if raw >= 0.0 {
        raw
    } else if raw >= -tolerance {
        0.0
    } else {
        f64::NAN
    }
}

pub(crate) fn predict_cart(model: &CartNode, features: &[f64]) -> Result<f64, String> {
    if features.iter().any(|value| !value.is_finite()) {
        return Err("invalid CART prediction inputs".to_string());
    }
    let prediction = match model {
        CartNode::Leaf { value, .. } => *value,
        CartNode::Split {
            feature_index,
            threshold,
            left,
            right,
        } => {
            if *feature_index >= features.len() || !threshold.is_finite() {
                return Err("invalid CART model".to_string());
            }
            if features[*feature_index] <= *threshold {
                predict_cart(left, features)?
            } else {
                predict_cart(right, features)?
            }
        }
    };
    prediction
        .is_finite()
        .then(|| normalize_zero(prediction))
        .ok_or_else(|| "CART prediction is non-finite".to_string())
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ridge_is_deterministic_and_constant_columns_are_safe() {
        let features = vec![
            vec![0.0, 2.0],
            vec![1.0, 2.0],
            vec![2.0, 2.0],
            vec![3.0, 2.0],
        ];
        let labels = vec![0.0, 1.0, 2.0, 3.0];
        let left = fit_ridge(&features, &labels, 0..3, 1.0e-6).unwrap();
        let right = fit_ridge(&features, &labels, 0..3, 1.0e-6).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.scales[1], 1.0);
        assert!((predict_ridge(&left, &features[3]).unwrap() - 3.0).abs() < 1.0e-3);
    }

    #[test]
    fn cart_uses_stable_threshold_routing_and_tie_breaking() {
        let features = vec![
            vec![0.0, 0.0],
            vec![1.0, 0.0],
            vec![2.0, 1.0],
            vec![3.0, 1.0],
        ];
        let labels = vec![0.0, 0.0, 1.0, 1.0];
        let model = fit_cart(
            &features,
            &labels,
            0..4,
            1,
            1,
            &["a".to_string(), "b".to_string()],
        )
        .unwrap();
        assert_eq!(
            predict_cart(&model, &[1.0, 0.0]).unwrap(),
            0.0,
            "<= threshold must route left"
        );
        assert_eq!(predict_cart(&model, &[2.0, 1.0]).unwrap(), 1.0);
        assert_eq!(sse_from_sums(1.0, 0.99999999999999, 1), 0.0);
    }
}
