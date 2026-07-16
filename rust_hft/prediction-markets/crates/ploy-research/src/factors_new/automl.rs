use crate::factors_new::registry::{FactorMeta, FactorRegistry};
use ploy_market_contracts::Regime;

#[derive(Debug, Clone, PartialEq)]
pub struct AutomlFactorAttribution {
    pub name: String,
    pub importance: f64,
    pub direction: i8,
    pub stability: f64,
}

pub fn register_automl_attributions(
    registry: &mut FactorRegistry,
    regime: Regime,
    label: &str,
    attributions: &[AutomlFactorAttribution],
) {
    for attribution in attributions {
        if !attribution.importance.is_finite() {
            continue;
        }
        let direction = if attribution.direction >= 0 { 1 } else { -1 };

        registry.insert(FactorMeta {
            name: format!("automl:{}", attribution.name),
            regime,
            label: label.to_string(),
            ic: attribution.importance.abs() * f64::from(direction),
            direction,
            stability: attribution.stability,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_automl_attributions_prefixes_and_sorts_through_registry() {
        let mut registry = FactorRegistry::new();
        register_automl_attributions(
            &mut registry,
            Regime::Late,
            "settlement_up",
            &[
                AutomlFactorAttribution {
                    name: "weak".to_string(),
                    importance: 0.02,
                    direction: 1,
                    stability: 0.1,
                },
                AutomlFactorAttribution {
                    name: "strong".to_string(),
                    importance: 0.20,
                    direction: -1,
                    stability: 0.8,
                },
            ],
        );

        let top = registry.top_n(Regime::Late, "settlement_up", 2);

        assert_eq!(top[0].name, "automl:strong");
        assert_eq!(top[0].direction, -1);
        assert_eq!(top[1].name, "automl:weak");
    }

    #[test]
    fn register_automl_attributions_skips_non_finite_scores() {
        let mut registry = FactorRegistry::new();
        register_automl_attributions(
            &mut registry,
            Regime::Late,
            "settlement_up",
            &[AutomlFactorAttribution {
                name: "nan".to_string(),
                importance: f64::NAN,
                direction: 1,
                stability: 0.0,
            }],
        );

        assert!(registry.all().is_empty());
    }
}
