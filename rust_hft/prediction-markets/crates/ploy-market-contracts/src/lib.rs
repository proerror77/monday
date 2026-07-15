//! Shared market event and feed contracts.

pub mod events;
pub mod family;
pub mod feed;
pub mod fees;
pub mod instrument;
pub mod venue;

pub use events::{
    l2_updates_from_depth_totals, market_update_sort_ts, normalize_token_id, BookLevel,
    MarketUpdate,
};
pub use family::PredictionFamily;
pub use feed::{BinanceSourceClock, BinanceSourceKind, Feed, HistoricalLoadOptions};
pub use fees::{
    polymarket_crypto_taker_fee_cost, polymarket_crypto_taker_fee_per_share, FeeAccumulator,
    FeeAsset, FeeCharge, FeeFormula, FeeRounding, FeeSchedule, FeeSettlement, LiquidityRole,
};
pub use instrument::InstrumentKind;
pub use venue::VenueKind;
