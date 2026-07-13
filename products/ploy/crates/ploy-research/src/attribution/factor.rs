use std::collections::BTreeMap;

/// Attribute P&L to factors by weighting each fill's P&L by |factor_value| / total_weight.
/// Input: Vec of (pnl, Vec<(factor_name, factor_value)>)
pub fn factor_pnl(fills: &[(f64, Vec<(String, f64)>)]) -> BTreeMap<String, f64> {
    let mut map: BTreeMap<String, f64> = BTreeMap::new();
    for (pnl, factors) in fills {
        let total_weight: f64 = factors.iter().map(|(_, v)| v.abs()).sum();
        if total_weight == 0.0 {
            continue;
        }
        for (name, value) in factors {
            *map.entry(name.clone()).or_default() += pnl * value.abs() / total_weight;
        }
    }
    map
}
