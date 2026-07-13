use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

#[must_use]
pub fn resolve_up_won(
    resolved: Option<bool>,
    spot: Option<Decimal>,
    price_to_beat: Option<Decimal>,
) -> Option<bool> {
    if resolved.is_some() {
        return resolved;
    }

    let spot = spot?.to_f64()?;
    let price_to_beat = price_to_beat?.to_f64()?;
    Some(spot >= price_to_beat)
}

#[cfg(test)]
mod tests {
    use super::resolve_up_won;
    use rust_decimal_macros::dec;

    #[test]
    fn explicit_resolution_wins_over_spot_fallback() {
        assert_eq!(
            resolve_up_won(Some(false), Some(dec!(120)), Some(dec!(100))),
            Some(false)
        );
    }

    #[test]
    fn falls_back_to_spot_price_to_beat_comparison() {
        assert_eq!(
            resolve_up_won(None, Some(dec!(120)), Some(dec!(100))),
            Some(true)
        );
        assert_eq!(
            resolve_up_won(None, Some(dec!(80)), Some(dec!(100))),
            Some(false)
        );
    }
}
