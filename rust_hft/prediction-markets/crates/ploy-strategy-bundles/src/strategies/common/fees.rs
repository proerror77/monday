#[must_use]
pub fn crypto_fee_cost(entry_price: f64) -> f64 {
    ploy_market_contracts::polymarket_crypto_taker_fee_per_share(entry_price)
}
