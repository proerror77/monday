use hft_core::{
    AssetClass, ComplianceContext, HftError, OrderType, Price, ProductType, Quantity, Side, Symbol,
    TimeInForce, VenueId,
};
use ports::{
    AccountView, BookLevel, ExecutionEvent, OrderIntent, RiskConfigSnapshot, RiskConfigUpdate,
    RiskManager, RiskMetrics, VenueSpec,
};
use std::collections::HashMap;

#[test]
fn ports_event_types_construct() {
    let _bl = BookLevel::new(1.0, 2.0);
}

struct PassThroughRisk;

impl RiskManager for PassThroughRisk {
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        _account: &AccountView,
        _venue_specs: &HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent> {
        intents
    }

    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        _account: &AccountView,
        _venue: &VenueSpec,
    ) -> Vec<OrderIntent> {
        intents
    }

    fn on_execution_event(&mut self, _event: &ExecutionEvent) {}

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, rust_decimal::Decimal> {
        HashMap::new()
    }

    fn should_halt_trading(&self, _account: &AccountView) -> bool {
        false
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: rust_decimal::Decimal::ZERO,
            current_drawdown: rust_decimal::Decimal::ZERO,
            var_1d: rust_decimal::Decimal::ZERO,
            leverage: rust_decimal::Decimal::ZERO,
            concentration_risk: rust_decimal::Decimal::ZERO,
            order_rate: rust_decimal::Decimal::ZERO,
            last_update: 0,
        }
    }

    fn update_config(&mut self, _update: RiskConfigUpdate) -> Result<(), HftError> {
        Ok(())
    }

    fn get_config_snapshot(&self) -> RiskConfigSnapshot {
        RiskConfigSnapshot::default()
    }
}

#[test]
fn tokenized_security_intent_needs_explicit_compliance_before_risk_review() {
    let mut risk = PassThroughRisk;
    let intent = OrderIntent {
        symbol: Symbol::new("TSLABUSDT"),
        asset_class: AssetClass::TokenizedSecurity,
        product_type: ProductType::TokenizedSecuritySpot,
        compliance_context: ComplianceContext::default(),
        side: Side::Buy,
        quantity: Quantity::from_f64(1.0).unwrap(),
        order_type: OrderType::Limit,
        price: Some(Price::from_f64(250.0).unwrap()),
        time_in_force: TimeInForce::GTC,
        strategy_id: "test".to_string(),
        target_venue: Some(VenueId::BINANCE_TOKENIZED_SECURITIES),
    };

    let mut specs = HashMap::new();
    specs.insert(VenueId::BINANCE_TOKENIZED_SECURITIES, VenueSpec::binance_spot());
    let approved = risk.review_with_venue_specs(vec![intent], &AccountView::default(), &specs);

    assert!(approved.is_empty());
}
