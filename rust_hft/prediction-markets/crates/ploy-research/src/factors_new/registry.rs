use ploy_market_contracts::Regime;

#[derive(Debug, Clone)]
pub struct FactorMeta {
    pub name: String,
    pub regime: Regime,
    pub label: String,
    pub ic: f64,
    pub direction: i8,
    pub stability: f64,
}

pub struct FactorRegistry {
    factors: Vec<FactorMeta>,
}

impl FactorRegistry {
    pub fn new() -> Self {
        Self {
            factors: Vec::new(),
        }
    }

    pub fn insert(&mut self, meta: FactorMeta) {
        self.factors.push(meta);
    }

    pub fn top_n(&self, regime: Regime, label: &str, n: usize) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self
            .factors
            .iter()
            .filter(|m| m.regime == regime && m.label == label)
            .collect();
        v.sort_by(|a, b| {
            b.ic.abs()
                .partial_cmp(&a.ic.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v.truncate(n);
        v
    }

    pub fn for_regime(&self, regime: Regime) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self.factors.iter().filter(|m| m.regime == regime).collect();
        v.sort_by(|a, b| {
            b.ic.abs()
                .partial_cmp(&a.ic.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    pub fn all(&self) -> &[FactorMeta] {
        &self.factors
    }
}

impl Default for FactorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_top_n_sorted_by_abs_ic() {
        let mut reg = FactorRegistry::new();
        reg.insert(FactorMeta {
            name: "a".into(),
            regime: Regime::Early,
            label: "settlement_up".into(),
            ic: 0.05,
            direction: 1,
            stability: 0.8,
        });
        reg.insert(FactorMeta {
            name: "b".into(),
            regime: Regime::Early,
            label: "settlement_up".into(),
            ic: 0.15,
            direction: -1,
            stability: 1.2,
        });
        let top = reg.top_n(Regime::Early, "settlement_up", 1);
        assert_eq!(top[0].name, "b");
    }
}
