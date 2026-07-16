use std::collections::HashMap;

use hft_core::{ProductType, Symbol, VenueId};
use ports::{AccountView, OrderIntent};
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExposureKey {
    venue: Option<VenueId>,
    product_type: ProductType,
    symbol: Symbol,
}

#[derive(Debug, Clone)]
pub(crate) struct ExposureProjection {
    pub symbol_gross_quantity: Decimal,
    pub gross_notional: Decimal,
}

/// Conservative projector for a batch of intents. Existing positions do not carry venue/product
/// attribution, so they cannot be proven reducible by an intent for a particular venue. They are
/// therefore retained as gross exposure; new exposure is isolated by venue + product + symbol.
#[derive(Debug, Clone)]
pub(crate) struct ExposureProjector {
    remaining_quantity: HashMap<Symbol, Decimal>,
    keyed_quantity: HashMap<ExposureKey, Decimal>,
    keyed_notional: HashMap<ExposureKey, Decimal>,
    gross_notional: Decimal,
}

impl ExposureProjector {
    pub(crate) fn new(account: &AccountView) -> Self {
        let remaining_quantity = account
            .positions
            .iter()
            .map(|(symbol, position)| (symbol.clone(), position.quantity.0))
            .collect();
        let remaining_notional: HashMap<_, _> = account
            .positions
            .iter()
            .map(|(symbol, position)| {
                (
                    symbol.clone(),
                    (position.avg_price.0 * position.quantity.0 + position.unrealized_pnl).abs(),
                )
            })
            .collect();
        let gross_notional = remaining_notional.values().copied().sum();
        Self {
            remaining_quantity,
            keyed_quantity: HashMap::new(),
            keyed_notional: HashMap::new(),
            gross_notional,
        }
    }

    pub(crate) fn project(
        &mut self,
        intent: &OrderIntent,
    ) -> Result<ExposureProjection, &'static str> {
        let price = intent
            .price
            .map(|price| price.0)
            .filter(|price| *price > Decimal::ZERO)
            .ok_or("projected exposure requires a positive executable price")?;
        let incoming = match intent.side {
            hft_core::Side::Buy => intent.quantity.0,
            hft_core::Side::Sell => -intent.quantity.0,
        };
        if incoming.is_zero() {
            return Err("projected exposure requires non-zero quantity");
        }

        if !incoming.is_zero() {
            let key = ExposureKey {
                venue: intent.target_venue,
                product_type: intent.product_type,
                symbol: intent.symbol.clone(),
            };
            let old_quantity = self.keyed_quantity.get(&key).copied().unwrap_or_default();
            let next_quantity = old_quantity + incoming;
            let old_notional = self.keyed_notional.get(&key).copied().unwrap_or_default();
            let next_notional = old_notional + incoming.abs() * price;
            self.gross_notional += next_notional - old_notional;
            self.keyed_quantity.insert(key.clone(), next_quantity);
            self.keyed_notional.insert(key, next_notional);
        }

        let unattributed = self
            .remaining_quantity
            .get(&intent.symbol)
            .copied()
            .unwrap_or_default()
            .abs();
        let keyed = self
            .keyed_quantity
            .iter()
            .filter(|(key, _)| key.symbol == intent.symbol)
            .map(|(_, quantity)| quantity.abs())
            .sum::<Decimal>();
        Ok(ExposureProjection {
            symbol_gross_quantity: unattributed + keyed,
            gross_notional: self.gross_notional,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Price, Quantity, Side, TimeInForce};

    fn intent(venue: VenueId, side: Side, quantity: i64) -> OrderIntent {
        OrderIntent::crypto_spot(
            Symbol::new("BTCUSDT"),
            side,
            Quantity(Decimal::from(quantity)),
            OrderType::Limit,
            Some(Price(Decimal::ONE)),
            TimeInForce::GTC,
            "alpha".to_string(),
            Some(venue),
        )
    }

    #[test]
    fn opposite_orders_on_different_venues_do_not_net() {
        let mut projector = ExposureProjector::new(&AccountView::default());
        projector
            .project(&intent(VenueId::BINANCE, Side::Buy, 60))
            .unwrap();
        let projected = projector
            .project(&intent(VenueId::BITGET, Side::Sell, 60))
            .unwrap();

        assert_eq!(projected.symbol_gross_quantity, Decimal::from(120));
        assert_eq!(projected.gross_notional, Decimal::from(120));
    }

    #[test]
    fn unattributed_existing_position_cannot_be_net_reduced() {
        let symbol = Symbol::new("BTCUSDT");
        let mut account = AccountView::default();
        account.positions.insert(
            symbol.clone(),
            ports::Position {
                symbol,
                quantity: Quantity(Decimal::from(60)),
                avg_price: Price(Decimal::ONE),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            },
        );
        let mut projector = ExposureProjector::new(&account);

        let projected = projector
            .project(&intent(VenueId::BITGET, Side::Sell, 60))
            .unwrap();

        assert_eq!(projected.symbol_gross_quantity, Decimal::from(120));
        assert_eq!(projected.gross_notional, Decimal::from(120));
    }

    #[test]
    fn later_low_price_does_not_revalue_prior_exposure_below_the_cap() {
        let mut projector = ExposureProjector::new(&AccountView::default());
        let mut expensive = intent(VenueId::BINANCE, Side::Buy, 100);
        expensive.price = Some(Price(Decimal::from(1_000)));
        let mut cheap = intent(VenueId::BINANCE, Side::Buy, 100);
        cheap.price = Some(Price(Decimal::ONE));

        assert_eq!(
            projector.project(&expensive).unwrap().gross_notional,
            Decimal::from(100_000)
        );
        assert_eq!(
            projector.project(&cheap).unwrap().gross_notional,
            Decimal::from(100_100)
        );
    }
}
