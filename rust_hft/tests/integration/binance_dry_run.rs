//! E2E dry-run using Binance quotes without network
use engine::aggregation::{AggregationEngine, MarketView, TopNSnapshot};
use hft_core::{VenueId, Symbol};
use ports::MarketEvent;

#[test]
fn binance_depth_update_dry_run() {
    // A minimal Binance depthUpdate message (streamed format)
    let depth_update = r#"{
        "e":"depthUpdate",
        "E":1700000123,
        "s":"BTCUSDT",
        "U":100,
        "u":105,
        "b":[["60000.00","0.5"]],
        "a":[["60001.00","0.4"]]
    }"#;

    // Parse into unified MarketEvent using the converter (simd-json enabled)
    let evt = data_adapter_binance::converter::MessageConverter::parse_direct_message(depth_update)
        .expect("parse ok")
        .expect("some event");

    // Feed into aggregation engine
    let mut agg = AggregationEngine::new();
    let mut out = Vec::new();
    agg.handle_event_into(evt, &mut out).expect("handle ok");

    // Build market view and assert we have best bid/ask for BINANCE:BTCUSDT
    let mv: MarketView = agg.build_market_view();
    let key = hft_core::VenueSymbol::new(VenueId::BINANCE, Symbol::new("BTCUSDT"));
    let ob = mv.get_orderbook(&key).expect("orderbook exists");

    assert_eq!(ob.bid_prices.len(), 1);
    assert_eq!(ob.ask_prices.len(), 1);
}

