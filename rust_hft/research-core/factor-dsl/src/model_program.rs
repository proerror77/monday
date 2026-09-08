//! Frozen fitted models over the same bounded factor interpreter used at runtime.
use crate::{
    evaluate_live_formula_series, live_evaluation_steps, validate_live_formula, FactorAst,
    LiveEventDomain, LiveFormulaCapability, MAX_LIVE_EVALUATION_STEPS,
};
use hft_research_manifest::model::{
    CexBaselineModelV1, CexDecisionCostsV1, CexSupervisedDecisionPolicyV2,
};
use serde::{Deserialize, Serialize};

pub const FROZEN_FACTOR_MODEL_SCHEMA_V1: &str = "monday.frozen_factor_model.v1";
pub const MAX_FROZEN_FACTORS: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenModelFactorV1 {
    pub ast: FactorAst,
    /// The original Factor Bank orientation, applied before model inference.
    pub negative: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenFactorModelV1 {
    pub schema_version: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub observation_frequency_millis: u64,
    pub label_horizon_buckets: usize,
    /// Exact training column order; never sort these during inference.
    pub factors: Vec<FrozenModelFactorV1>,
    pub model: CexBaselineModelV1,
    pub decision_policy: CexSupervisedDecisionPolicyV2,
    /// Frozen policy costs excluding observed half-spread, in basis points.
    pub base_costs: CexDecisionCostsV1,
    pub cross_spread: bool,
}

impl FrozenFactorModelV1 {
    pub fn validate(&self) -> Result<LiveFormulaCapability, String> {
        if self.schema_version != FROZEN_FACTOR_MODEL_SCHEMA_V1
            || self.factors.is_empty()
            || self.factors.len() > MAX_FROZEN_FACTORS
        {
            return Err("invalid frozen factor model schema or feature count".into());
        }
        if self.observation_frequency_millis == 0
            || self.label_horizon_buckets == 0
            || self
                .observation_frequency_millis
                .checked_mul(1_000)
                .is_none()
            || u64::try_from(self.label_horizon_buckets)
                .ok()
                .and_then(|horizon| horizon.checked_mul(self.observation_frequency_millis))
                .is_none()
        {
            return Err("invalid frozen model observation or label clock".into());
        }
        if self.venue.is_empty()
            || self.venue.len() > 32
            || !self
                .venue
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
            || !matches!(self.market.as_str(), "spot" | "usdm")
            || self.symbol.is_empty()
            || self.symbol.len() > 64
            || !self
                .symbol
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"-_".contains(&b))
        {
            return Err("invalid frozen model instrument".into());
        }
        if let CexBaselineModelV1::BurnMlpPortable { symbol, venue, .. } = &self.model {
            if symbol != &self.symbol || venue != &format!("{}-{}", self.venue, self.market) {
                return Err("frozen MLP instrument differs from training".into());
            }
        }
        self.model.validate_inference(self.factors.len())?;
        self.decision_policy.validate()?;
        if !self.base_costs.one_way_cost_bps.is_finite()
            || !self.base_costs.funding_bps.is_finite()
            || self.base_costs.funding_bps < 0.0
        {
            return Err("invalid frozen decision costs".into());
        }
        let mut history_rows = 1;
        for factor in &self.factors {
            let capability =
                validate_live_formula(&factor.ast).map_err(|error| error.to_string())?;
            if capability.event_domain != LiveEventDomain::Snapshot {
                return Err("frozen CEX models require snapshot-domain factors".into());
            }
            history_rows = history_rows.max(capability.history_rows);
        }
        let mut steps = 0usize;
        for factor in &self.factors {
            steps = steps
                .checked_add(
                    live_evaluation_steps(&factor.ast, history_rows)
                        .ok_or("frozen factor evaluation steps overflow")?,
                )
                .ok_or("frozen factor model evaluation budget overflow")?;
        }
        if steps > MAX_LIVE_EVALUATION_STEPS {
            return Err("frozen factor model exceeds evaluation budget".into());
        }
        Ok(LiveFormulaCapability {
            event_domain: LiveEventDomain::Snapshot,
            history_rows,
        })
    }

    pub fn predict_from_history(
        &self,
        row_count: usize,
        field_value: impl Fn(usize, &str) -> Option<f64>,
    ) -> Result<f64, String> {
        let capability = self.validate()?;
        let offset = row_count
            .checked_sub(capability.history_rows)
            .ok_or("frozen factor model requires more history")?;
        let mut features = Vec::with_capacity(self.factors.len());
        for factor in &self.factors {
            let values =
                evaluate_live_formula_series(&factor.ast, capability.history_rows, |row, field| {
                    field_value(offset + row, field)
                })
                .map_err(|error| error.to_string())?;
            let value = *values.last().ok_or("missing frozen factor value")?;
            let value = if factor.negative { -value } else { value };
            features.push(if value == 0.0 { 0.0 } else { value });
        }
        self.model.predict(&features)
    }

    pub fn target_position(
        &self,
        prediction: f64,
        previous: f64,
        spread_bps: f64,
    ) -> Result<f64, String> {
        if !spread_bps.is_finite() || spread_bps < 0.0 {
            return Err("invalid observed model spread".into());
        }
        let mut costs = self.base_costs;
        if self.cross_spread {
            costs.one_way_cost_bps += spread_bps / 2.0;
        }
        self.decision_policy
            .target_position(prediction, previous, costs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FactorOperator, FactorTerminal};

    fn fixture() -> FrozenFactorModelV1 {
        FrozenFactorModelV1 {
            schema_version: FROZEN_FACTOR_MODEL_SCHEMA_V1.into(),
            venue: "binance".into(),
            market: "usdm".into(),
            symbol: "BTCUSDT".into(),
            observation_frequency_millis: 1_000,
            label_horizon_buckets: 5,
            factors: vec![FrozenModelFactorV1 {
                ast: FactorAst::call(
                    FactorOperator::Delta,
                    vec![
                        FactorAst::Terminal(FactorTerminal::Field("book_imbalance".into())),
                        FactorAst::Terminal(FactorTerminal::Constant("1".into())),
                    ],
                )
                .unwrap(),
                negative: true,
            }],
            model: CexBaselineModelV1::Ridge {
                intercept: 0.0,
                means: vec![0.0],
                scales: vec![1.0],
                coefficients: vec![0.25],
            },
            decision_policy: CexSupervisedDecisionPolicyV2::prediction_identity_v2(),
            base_costs: CexDecisionCostsV1 {
                one_way_cost_bps: 2.5,
                funding_bps: 0.0,
            },
            cross_spread: true,
        }
    }

    #[test]
    fn frozen_model_reuses_causal_factor_history_and_orientation() {
        let model = fixture();
        assert_eq!(model.validate().unwrap().history_rows, 2);
        let values = [999.0, 1.0, 3.0];
        assert_eq!(
            model
                .predict_from_history(3, |row, field| (field == "book_imbalance")
                    .then_some(values[row]))
                .unwrap(),
            -0.5
        );
        assert!(model.predict_from_history(1, |_, _| Some(1.0)).is_err());
        assert!(model.predict_from_history(3, |_, _| None).is_err());
        assert_eq!(model.target_position(-0.5, 0.0, 2.0).unwrap(), -0.5);
        let mut unsupported = model.clone();
        unsupported.factors[0].ast = FactorAst::Terminal(FactorTerminal::Field(
            "aggregate_trade_flow_imbalance".into(),
        ));
        assert!(
            unsupported.validate().is_err(),
            "unimplemented live fields cannot be silently zero-filled"
        );
        unsupported = model;
        unsupported.factors.clear();
        assert!(unsupported.validate().is_err());
    }
}
