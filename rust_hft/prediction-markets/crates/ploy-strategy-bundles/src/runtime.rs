use crate::bundle::StrategyBundle;
use crate::signals::MarketSignal;
use chrono::Utc;
use ploy_trading::TradingIntent;

pub fn emit_intents(
    deployment_id: &str,
    bundle: &StrategyBundle,
    market_signal: &MarketSignal,
) -> Vec<TradingIntent> {
    let now = Utc::now();
    bundle
        .signals
        .iter()
        .filter_map(|config| config.evaluate(deployment_id, market_signal, now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::emit_intents;
    use crate::bundle::StrategyBundle;
    use crate::signals::{MarketSignal, SignalConfig};
    use rust_decimal_macros::dec;

    #[test]
    fn bundle_emits_multiple_intents() {
        let bundle = StrategyBundle {
            bundle_id: "openclaw".to_string(),
            signals: vec![
                SignalConfig::ThresholdEntry {
                    market_id: "market-1".to_string(),
                    token_id: "yes-token".to_string(),
                    threshold_bps: 100,
                    quantity: dec!(2),
                },
                SignalConfig::ThresholdEntry {
                    market_id: "market-1".to_string(),
                    token_id: "yes-token".to_string(),
                    threshold_bps: 200,
                    quantity: dec!(1),
                },
            ],
        };

        let intents = emit_intents(
            "openclaw.default",
            &bundle,
            &MarketSignal {
                market_id: "market-1".to_string(),
                token_id: "yes-token".to_string(),
                strength_bps: 250,
            },
        );

        assert_eq!(intents.len(), 2);
    }
}
