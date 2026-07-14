use crate::config::FullConfig;
use crate::strategies::{
    diff_enhanced::DiffEnhancedConfig, diff_regular::DiffRegularConfig,
    prob_chase::ProbChaseConfig, prob_reversal::ProbReversalConfig, sweep::SweepConfig,
};
use crate::traits::StrategyLogic;
use crate::{
    BayesianDirectionalStrategy, DiffEnhancedStrategy, DiffRegularStrategy, DirectionalStrategy,
    MeanReversionStrategy, ProbChaseStrategy, ProbReversalStrategy, ReversalStrategy,
    SweepStrategy, ThreeLayerStrategy,
};
use rust_decimal::Decimal;
use tracing::info;

#[must_use]
pub fn canonical_strategy_variant(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "directional" | "v1" | "v2" | "v3" | "pm5d_v1" | "pm5d_v2" | "pm5d_v3" => {
            "directional".to_string()
        }
        "directional_bayes" | "directional-bayes" | "pm5d_bayes" => "directional_bayes".to_string(),
        "mean_reversion" | "mean-reversion" | "pm5d_v4" | "v4" => "mean_reversion".to_string(),
        "reversal" | "pm5d_reversal" | "pm-5m-reversal" => "reversal".to_string(),
        "three_layer" | "three-layer" | "threelayer" => "three_layer".to_string(),
        "diff_enhanced" | "diff-enhanced" | "s1" | "s1_enhanced" => "diff_enhanced".to_string(),
        "diff_regular" | "diff-regular" | "s2" | "s2_regular" => "diff_regular".to_string(),
        "sweep" | "s3" | "s3_sweep" => "sweep".to_string(),
        "prob_reversal" | "prob-reversal" | "s4" | "s4_reversal" => "prob_reversal".to_string(),
        "prob_chase" | "prob-chase" | "s5" | "s5_prob_chase" => "prob_chase".to_string(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyKind {
    Directional,
    DirectionalBayes,
    MeanReversion,
    Reversal,
    ThreeLayer,
    DiffEnhanced,
    DiffRegular,
    Sweep,
    ProbReversal,
    ProbChase,
    Unknown(String),
}

impl StrategyKind {
    #[must_use]
    pub fn from_raw(raw: &str) -> Self {
        match canonical_strategy_variant(raw).as_str() {
            "directional" => Self::Directional,
            "directional_bayes" => Self::DirectionalBayes,
            "mean_reversion" => Self::MeanReversion,
            "reversal" => Self::Reversal,
            "three_layer" => Self::ThreeLayer,
            "diff_enhanced" => Self::DiffEnhanced,
            "diff_regular" => Self::DiffRegular,
            "sweep" => Self::Sweep,
            "prob_reversal" => Self::ProbReversal,
            "prob_chase" => Self::ProbChase,
            other => Self::Unknown(other.to_string()),
        }
    }

    #[must_use]
    pub fn canonical_name(&self) -> &str {
        match self {
            Self::Directional => "directional",
            Self::DirectionalBayes => "directional_bayes",
            Self::MeanReversion => "mean_reversion",
            Self::Reversal => "reversal",
            Self::ThreeLayer => "three_layer",
            Self::DiffEnhanced => "diff_enhanced",
            Self::DiffRegular => "diff_regular",
            Self::Sweep => "sweep",
            Self::ProbReversal => "prob_reversal",
            Self::ProbChase => "prob_chase",
            Self::Unknown(name) => name.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StrategyConfigEnvelope<T> {
    pub kind: StrategyKind,
    pub config: T,
}

impl FullConfig {
    #[must_use]
    pub fn strategy_kind(&self) -> StrategyKind {
        StrategyKind::from_raw(&self.runtime.strategy_variant)
    }

    #[must_use]
    pub fn strategy_config_envelope(
        &self,
    ) -> StrategyConfigEnvelope<crate::strategies::directional::DirectionalConfig> {
        StrategyConfigEnvelope {
            kind: self.strategy_kind(),
            config: self.strategy.clone(),
        }
    }
}

pub fn build_strategy(config: &FullConfig) -> Box<dyn StrategyLogic> {
    let configured_variant = config.runtime.strategy_variant.trim();
    let canonical_variant = config.runtime.canonical_strategy_variant();

    match canonical_variant.as_str() {
        "directional" => {
            if configured_variant != "directional" {
                info!(
                    configured_variant = configured_variant,
                    canonical_variant = canonical_variant.as_str(),
                    "Using roadmap alias for directional strategy variant",
                );
            }

            Box::new(DirectionalStrategy::new(config.strategy.clone()))
        }
        "directional_bayes" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using Bayesian directional strategy variant",
            );
            let json = serde_json::to_value(&config.strategy).expect("serialize DirectionalConfig");
            let bayes_config: crate::strategies::directional_bayes::BayesianDirectionalConfig =
                serde_json::from_value(json).expect("deserialize BayesianDirectionalConfig");
            Box::new(BayesianDirectionalStrategy::new(bayes_config))
        }
        "mean_reversion" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using mean-reversion strategy variant",
            );
            Box::new(MeanReversionStrategy::new(config.strategy.clone()))
        }
        "reversal" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using reversal strategy variant",
            );
            Box::new(ReversalStrategy::new(config.strategy.clone().into()))
        }
        "three_layer" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using three-layer directional strategy variant",
            );
            let mut three_layer_config =
                crate::strategies::three_layer::ThreeLayerConfig::from_directional_runtime(
                    config.strategy.clone(),
                )
                .unwrap_or_else(|err| {
                    eprintln!("Invalid three-layer strategy config: {err}");
                    std::process::exit(2);
                });
            three_layer_config.visible_depth_haircut =
                Decimal::try_from(config.execution.visible_depth_haircut).unwrap_or(Decimal::ONE);
            three_layer_config.max_sweep_levels = config.execution.max_sweep_levels;
            three_layer_config.max_sweep_price_delta =
                Decimal::try_from(config.execution.max_sweep_price_delta).unwrap_or_default();
            Box::new(ThreeLayerStrategy::new(three_layer_config))
        }
        "diff_enhanced" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using diff-enhanced strategy variant",
            );
            Box::new(DiffEnhancedStrategy::new(DiffEnhancedConfig::from(
                config.strategy.clone(),
            )))
        }
        "diff_regular" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using diff-regular strategy variant",
            );
            Box::new(DiffRegularStrategy::new(DiffRegularConfig::from(
                config.strategy.clone(),
            )))
        }
        "sweep" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using sweep strategy variant",
            );
            Box::new(SweepStrategy::new(SweepConfig::from(
                config.strategy.clone(),
            )))
        }
        "prob_reversal" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using probability-reversal strategy variant",
            );
            Box::new(ProbReversalStrategy::new(ProbReversalConfig::from(
                config.strategy.clone(),
            )))
        }
        "prob_chase" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using probability-chase strategy variant",
            );
            Box::new(ProbChaseStrategy::new(ProbChaseConfig::from(
                config.strategy.clone(),
            )))
        }
        _ => {
            eprintln!(
                "Unsupported strategy_variant `{configured_variant}` in config. \
                 Supported runtime variants: directional, directional_bayes, mean_reversion, reversal, three_layer, diff_enhanced, diff_regular, sweep, prob_reversal, prob_chase, v1, v2, v3, v4, s1, s2, s3, s4, s5."
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_strategy, canonical_strategy_variant, StrategyKind};
    use crate::FullConfig;

    #[test]
    fn canonical_strategy_variant_normalizes_roadmap_aliases() {
        for (raw, expected) in [
            ("directional", "directional"),
            ("v1", "directional"),
            ("v2", "directional"),
            ("v3", "directional"),
            ("directional_bayes", "directional_bayes"),
            ("v4", "mean_reversion"),
            ("pm5d_v4", "mean_reversion"),
            ("reversal", "reversal"),
            ("pm5d_reversal", "reversal"),
        ] {
            assert_eq!(canonical_strategy_variant(raw), expected);
        }
    }

    #[test]
    fn strategy_kind_exposes_canonical_kind() {
        assert_eq!(StrategyKind::from_raw("v4"), StrategyKind::MeanReversion);
        assert_eq!(StrategyKind::from_raw("s5"), StrategyKind::ProbChase);
        assert_eq!(
            StrategyKind::from_raw("unknown"),
            StrategyKind::Unknown("unknown".to_string())
        );
    }

    #[test]
    fn full_config_exposes_strategy_config_envelope() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
strategy_variant = "reversal"

[strategy]
stake_usd = 12.0
"#,
        )
        .unwrap();
        let envelope = config.strategy_config_envelope();
        assert_eq!(envelope.kind, StrategyKind::Reversal);
        assert_eq!(envelope.config.stake_usd, rust_decimal::Decimal::new(12, 0));
    }

    #[test]
    fn roadmap_aliases_build_expected_strategy_variants() {
        for (variant, expected_name) in [
            ("v1", "pm_5m_directional"),
            ("v2", "pm_5m_directional"),
            ("v3", "pm_5m_directional"),
            ("v4", "pm_5m_mean_reversion"),
            ("reversal", "pm_5m_reversal"),
            ("three_layer", "three_layer"),
            ("three-layer", "three_layer"),
            ("diff_enhanced", "diff_enhanced"),
            ("diff_regular", "diff_regular"),
            ("sweep", "sweep"),
            ("prob_reversal", "prob_reversal"),
            ("prob_chase", "prob_chase"),
            ("s1", "diff_enhanced"),
            ("s2", "diff_regular"),
            ("s3", "sweep"),
            ("s4", "prob_reversal"),
            ("s5", "prob_chase"),
        ] {
            let config = FullConfig::from_toml(&format!(
                r#"
[runtime]
mode = "dryrun"
strategy_variant = "{variant}"

[strategy]
"#
            ))
            .unwrap();

            let strategy = build_strategy(&config);
            assert_eq!(strategy.name(), expected_name);
        }
    }
}
