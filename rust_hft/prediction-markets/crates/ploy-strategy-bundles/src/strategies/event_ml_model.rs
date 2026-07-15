use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

const SUPPORTED_MODEL_KIND: &str = "event_ml_logistic_baseline_model";
const SUPPORTED_MODEL_VERSION: u32 = 1;
const SUPPORTED_MODEL_FAMILY: &str = "logistic_regression";
const SUPPORTED_TARGET_LABEL: &str = "settlement_up";
pub const EVENT_ML_RUNTIME_SCORE_PREFIX: &str = "event_ml_model:";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EventMlBaselineArtifact {
    pub model: EventMlModelContract,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EventMlModelContract {
    pub kind: String,
    pub version: u32,
    pub family: String,
    pub target_label: String,
    pub feature_schema: Vec<String>,
    pub intercept: f64,
    pub weights: Vec<EventMlFeatureWeight>,
    pub standardizer: EventMlStandardizer,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EventMlFeatureWeight {
    pub feature: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EventMlStandardizer {
    pub method: String,
    pub fit_split: String,
    pub features: Vec<EventMlFeatureStandardizer>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EventMlFeatureStandardizer {
    pub feature: String,
    pub mean: f64,
    pub std: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EventMlScore {
    pub logit: f64,
    pub probability: f64,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EventMlModelError {
    #[error("unsupported Event ML model kind/version/family/target")]
    UnsupportedModel,
    #[error("feature schema, weights, and standardizer features must have identical length")]
    FeatureLengthMismatch,
    #[error("model feature order mismatch at index {index}: expected {expected}, got {actual}")]
    FeatureOrderMismatch {
        index: usize,
        expected: String,
        actual: String,
    },
    #[error("standardizer must use train-fitted zscore normalization")]
    UnsupportedStandardizer,
    #[error("model contains non-finite parameter for feature {feature}")]
    NonFiniteParameter { feature: String },
    #[error("missing feature value: {0}")]
    MissingFeature(String),
    #[error("non-finite feature value: {0}")]
    NonFiniteFeature(String),
}

impl EventMlModelContract {
    pub fn validate(&self) -> Result<(), EventMlModelError> {
        if self.kind != SUPPORTED_MODEL_KIND
            || self.version != SUPPORTED_MODEL_VERSION
            || self.family != SUPPORTED_MODEL_FAMILY
            || self.target_label != SUPPORTED_TARGET_LABEL
        {
            return Err(EventMlModelError::UnsupportedModel);
        }
        if self.standardizer.method != "zscore" || self.standardizer.fit_split != "train" {
            return Err(EventMlModelError::UnsupportedStandardizer);
        }
        let len = self.feature_schema.len();
        if self.weights.len() != len || self.standardizer.features.len() != len {
            return Err(EventMlModelError::FeatureLengthMismatch);
        }
        if !self.intercept.is_finite() {
            return Err(EventMlModelError::NonFiniteParameter {
                feature: "intercept".to_string(),
            });
        }
        for (idx, feature) in self.feature_schema.iter().enumerate() {
            let weight = &self.weights[idx];
            if weight.feature != *feature {
                return Err(EventMlModelError::FeatureOrderMismatch {
                    index: idx,
                    expected: feature.clone(),
                    actual: weight.feature.clone(),
                });
            }
            let standardizer = &self.standardizer.features[idx];
            if standardizer.feature != *feature {
                return Err(EventMlModelError::FeatureOrderMismatch {
                    index: idx,
                    expected: feature.clone(),
                    actual: standardizer.feature.clone(),
                });
            }
            if !weight.weight.is_finite()
                || !standardizer.mean.is_finite()
                || !standardizer.std.is_finite()
                || standardizer.std <= 0.0
            {
                return Err(EventMlModelError::NonFiniteParameter {
                    feature: feature.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn score_ordered(&self, values: &[f64]) -> Result<EventMlScore, EventMlModelError> {
        self.validate()?;
        if values.len() != self.feature_schema.len() {
            return Err(EventMlModelError::FeatureLengthMismatch);
        }
        let mut logit = self.intercept;
        for (idx, value) in values.iter().enumerate() {
            let feature = &self.feature_schema[idx];
            if !value.is_finite() {
                return Err(EventMlModelError::NonFiniteFeature(feature.clone()));
            }
            let standardizer = &self.standardizer.features[idx];
            let normalized = (*value - standardizer.mean) / standardizer.std;
            logit += self.weights[idx].weight * normalized;
        }
        Ok(EventMlScore {
            logit,
            probability: sigmoid(logit),
        })
    }

    pub fn score_map(
        &self,
        values: &BTreeMap<String, f64>,
    ) -> Result<EventMlScore, EventMlModelError> {
        let ordered = self
            .feature_schema
            .iter()
            .map(|feature| {
                values
                    .get(feature)
                    .copied()
                    .ok_or_else(|| EventMlModelError::MissingFeature(feature.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.score_ordered(&ordered)
    }
}

pub fn parse_event_ml_baseline_model(
    json: &str,
) -> Result<EventMlModelContract, serde_json::Error> {
    serde_json::from_str::<EventMlBaselineArtifact>(json).map(|artifact| artifact.model)
}

#[must_use]
pub fn is_event_ml_runtime_score(runtime_score: &str) -> bool {
    runtime_score
        .trim()
        .starts_with(EVENT_ML_RUNTIME_SCORE_PREFIX)
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> EventMlModelContract {
        EventMlModelContract {
            kind: SUPPORTED_MODEL_KIND.to_string(),
            version: SUPPORTED_MODEL_VERSION,
            family: SUPPORTED_MODEL_FAMILY.to_string(),
            target_label: SUPPORTED_TARGET_LABEL.to_string(),
            feature_schema: vec!["distance".to_string(), "edge".to_string()],
            intercept: 0.25,
            weights: vec![
                EventMlFeatureWeight {
                    feature: "distance".to_string(),
                    weight: 0.5,
                },
                EventMlFeatureWeight {
                    feature: "edge".to_string(),
                    weight: -0.75,
                },
            ],
            standardizer: EventMlStandardizer {
                method: "zscore".to_string(),
                fit_split: "train".to_string(),
                features: vec![
                    EventMlFeatureStandardizer {
                        feature: "distance".to_string(),
                        mean: 1.0,
                        std: 2.0,
                    },
                    EventMlFeatureStandardizer {
                        feature: "edge".to_string(),
                        mean: 3.0,
                        std: 4.0,
                    },
                ],
            },
        }
    }

    #[test]
    fn scores_ordered_features_with_train_standardizer() {
        let model = contract();

        let score = model.score_ordered(&[3.0, 7.0]).expect("score");

        let expected_logit = 0.25 + 0.5 * ((3.0 - 1.0) / 2.0) - 0.75 * ((7.0 - 3.0) / 4.0);
        assert!((score.logit - expected_logit).abs() < 1e-12);
        assert!((score.probability - sigmoid(expected_logit)).abs() < 1e-12);
    }

    #[test]
    fn scores_feature_map_in_schema_order() {
        let model = contract();
        let values = BTreeMap::from([("edge".to_string(), 7.0), ("distance".to_string(), 3.0)]);

        let from_map = model.score_map(&values).expect("map score");
        let from_ordered = model.score_ordered(&[3.0, 7.0]).expect("ordered score");

        assert_eq!(from_map, from_ordered);
    }

    #[test]
    fn rejects_mismatched_feature_order() {
        let mut model = contract();
        model.weights[1].feature = "wrong".to_string();

        assert!(matches!(
            model.validate(),
            Err(EventMlModelError::FeatureOrderMismatch { index: 1, .. })
        ));
    }

    #[test]
    fn parses_model_from_baseline_metrics_json() {
        let raw = serde_json::json!({
            "model": {
                "kind": SUPPORTED_MODEL_KIND,
                "version": SUPPORTED_MODEL_VERSION,
                "family": SUPPORTED_MODEL_FAMILY,
                "target_label": SUPPORTED_TARGET_LABEL,
                "feature_schema": ["distance", "edge"],
                "intercept": 0.25,
                "weights": [
                    {"feature": "distance", "weight": 0.5},
                    {"feature": "edge", "weight": -0.75}
                ],
                "standardizer": {
                    "method": "zscore",
                    "fit_split": "train",
                    "features": [
                        {"feature": "distance", "mean": 1.0, "std": 2.0},
                        {"feature": "edge", "mean": 3.0, "std": 4.0}
                    ]
                }
            }
        })
        .to_string();

        let model = parse_event_ml_baseline_model(&raw).expect("parse");

        assert_eq!(model, contract());
        assert!(model.validate().is_ok());
    }
}
