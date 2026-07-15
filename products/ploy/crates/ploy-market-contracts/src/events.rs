use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BookLevel {
    pub price: Decimal,
    pub size: Decimal,
}

/// Unified market update consumed by market data, strategy, and research code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MarketUpdate {
    /// CEX spot price tick.
    SpotPrice {
        symbol: Arc<str>,
        price: Decimal,
        ts: DateTime<Utc>,
    },

    /// CEX aggregated trade tick with aggressor-side metadata.
    AggTrade {
        symbol: Arc<str>,
        agg_trade_id: u64,
        price: Decimal,
        quantity: Decimal,
        is_buyer_maker: bool,
        ts: DateTime<Utc>,
    },

    /// Polymarket token quote update.
    Quote {
        token_id: Arc<str>,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
        #[serde(default)]
        bid_levels: Vec<BookLevel>,
        #[serde(default)]
        ask_levels: Vec<BookLevel>,
        ts: DateTime<Utc>,
    },

    /// CEX L2 orderbook summary.
    L2 {
        symbol: Arc<str>,
        obi: f64,
        spread_bps: u32,
        ts: DateTime<Utc>,
    },

    /// CEX L2 orderbook summary with near-mid depth totals.
    L2Depth {
        symbol: Arc<str>,
        obi: f64,
        spread_bps: u32,
        bid_depth_near: f64,
        ask_depth_near: f64,
        ts: DateTime<Utc>,
    },

    /// New binary-option event window discovered.
    EventDiscovered {
        event_id: Arc<str>,
        symbol: Arc<str>,
        up_token: Arc<str>,
        down_token: Arc<str>,
        end_time: DateTime<Utc>,
        window_secs: u64,
        price_to_beat: Option<Decimal>,
        resolved_up_won: Option<bool>,
    },

    /// Event window expired.
    EventExpired {
        event_id: Arc<str>,
        end_time: DateTime<Utc>,
        resolved_up_won: Option<bool>,
    },

    /// External sports game state update from the legacy sports feed.
    SportsState {
        game_id: Arc<str>,
        league: Arc<str>,
        slug: Arc<str>,
        home_team: Arc<str>,
        away_team: Arc<str>,
        status: Arc<str>,
        period: Option<Arc<str>>,
        score: Option<Arc<str>>,
        elapsed: Option<Arc<str>>,
        live: bool,
        ended: bool,
        finished_at: Option<DateTime<Utc>>,
        ts: DateTime<Utc>,
    },

    /// Pre-game sports state: schedule, teams, odds, and optional model probability.
    SportsPregame {
        game_id: Arc<str>,
        league: Arc<str>,
        home_team: Arc<str>,
        away_team: Arc<str>,
        start_time: DateTime<Utc>,
        home_odds: f64,
        away_odds: f64,
        model_home_prob: Option<f64>,
        ts: DateTime<Utc>,
    },

    /// Live sports state: score, clock, and momentum snapshot.
    SportsLive {
        game_id: Arc<str>,
        league: Arc<str>,
        period: Arc<str>,
        home_score: u32,
        away_score: u32,
        clock_remaining_secs: Option<u32>,
        momentum: f64,
        ts: DateTime<Utc>,
    },

    /// Reference-price tick from Chainlink, Pyth, or another canonical source.
    ReferencePrice {
        symbol: Arc<str>,
        source: Arc<str>,
        asset_class: Arc<str>,
        price: Decimal,
        full_accuracy_value: Option<Arc<str>>,
        is_carried_forward: bool,
        /// Time this process or upstream capture received the tick.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        received_at: Option<DateTime<Utc>>,
        /// Source timestamp supplied by the reference-price provider.
        ts: DateTime<Utc>,
    },

    /// CEX kline close.
    Kline {
        symbol: Arc<str>,
        interval: Arc<str>,
        open: Decimal,
        close: Decimal,
        volume: Decimal,
        ts: DateTime<Utc>,
    },
}

#[must_use]
pub fn l2_updates_from_depth_totals(
    symbol: &str,
    obi: f64,
    spread_bps: u32,
    bid_depth_near: Decimal,
    ask_depth_near: Decimal,
    ts: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let symbol: Arc<str> = Arc::from(symbol);
    vec![
        MarketUpdate::L2 {
            symbol: Arc::clone(&symbol),
            obi,
            spread_bps,
            ts,
        },
        MarketUpdate::L2Depth {
            symbol,
            obi,
            spread_bps,
            bid_depth_near: bid_depth_near.to_f64().unwrap_or(0.0),
            ask_depth_near: ask_depth_near.to_f64().unwrap_or(0.0),
            ts,
        },
    ]
}

impl MarketUpdate {
    /// Timestamp used to merge heterogeneous historical updates.
    ///
    /// `EventDiscovered` deliberately sorts before its quote window because
    /// Polymarket quotes can arrive before the official start timestamp.
    #[must_use]
    pub fn sort_ts(&self) -> DateTime<Utc> {
        match self {
            MarketUpdate::SpotPrice { ts, .. }
            | MarketUpdate::AggTrade { ts, .. }
            | MarketUpdate::Quote { ts, .. }
            | MarketUpdate::L2 { ts, .. }
            | MarketUpdate::L2Depth { ts, .. }
            | MarketUpdate::SportsState { ts, .. }
            | MarketUpdate::SportsPregame { ts, .. }
            | MarketUpdate::SportsLive { ts, .. }
            | MarketUpdate::Kline { ts, .. } => *ts,
            MarketUpdate::ReferencePrice {
                received_at, ts, ..
            } => received_at.unwrap_or(*ts),
            MarketUpdate::EventDiscovered {
                end_time,
                window_secs,
                ..
            } => {
                *end_time
                    - chrono::Duration::seconds(*window_secs as i64)
                    - chrono::Duration::hours(1)
            }
            MarketUpdate::EventExpired { end_time, .. } => *end_time,
        }
    }
}

#[must_use]
pub fn market_update_sort_ts(update: &MarketUpdate) -> DateTime<Utc> {
    update.sort_ts()
}

#[must_use]
pub fn normalize_token_id(raw: &str) -> String {
    let value = raw.trim().trim_matches('"');
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return hex_to_decimal_string(hex).unwrap_or_else(|| value.to_string());
    }
    value.to_string()
}

fn hex_to_decimal_string(hex: &str) -> Option<String> {
    if hex.is_empty() {
        return None;
    }

    let mut digits = vec![0_u8];

    for ch in hex.chars() {
        let value = ch.to_digit(16)? as u32;
        let mut carry = value;

        for digit in &mut digits {
            let next = (*digit as u32) * 16 + carry;
            *digit = (next % 10) as u8;
            carry = next / 10;
        }

        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }

    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }

    Some(
        digits
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + *digit))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{market_update_sort_ts, normalize_token_id, MarketUpdate};
    use crate::{InstrumentKind, PredictionFamily, VenueKind};
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use std::sync::Arc;

    fn ts() -> DateTime<Utc> {
        "2026-04-21T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn existing_market_update_kind_tags_stay_stable() {
        let cases = [
            (
                "spot_price",
                MarketUpdate::SpotPrice {
                    symbol: Arc::from("BTCUSDT"),
                    price: Decimal::new(10_000, 2),
                    ts: ts(),
                },
            ),
            (
                "quote",
                MarketUpdate::Quote {
                    token_id: Arc::from("token"),
                    bid: Some(Decimal::new(45, 2)),
                    ask: Some(Decimal::new(55, 2)),
                    bid_size: None,
                    ask_size: None,
                    bid_levels: Vec::new(),
                    ask_levels: Vec::new(),
                    ts: ts(),
                },
            ),
            (
                "sports_state",
                MarketUpdate::SportsState {
                    game_id: Arc::from("game"),
                    league: Arc::from("nba"),
                    slug: Arc::from("nba-game"),
                    home_team: Arc::from("Home"),
                    away_team: Arc::from("Away"),
                    status: Arc::from("scheduled"),
                    period: None,
                    score: None,
                    elapsed: None,
                    live: false,
                    ended: false,
                    finished_at: None,
                    ts: ts(),
                },
            ),
        ];

        for (expected, update) in cases {
            let value = serde_json::to_value(update).unwrap();
            assert_eq!(value["kind"], expected);
        }
    }

    #[test]
    fn legacy_sports_state_round_trips() {
        let update = MarketUpdate::SportsState {
            game_id: Arc::from("game"),
            league: Arc::from("nba"),
            slug: Arc::from("nba-game"),
            home_team: Arc::from("Home"),
            away_team: Arc::from("Away"),
            status: Arc::from("inprogress"),
            period: Some(Arc::from("Q4")),
            score: Some(Arc::from("101-99")),
            elapsed: Some(Arc::from("47:30")),
            live: true,
            ended: false,
            finished_at: None,
            ts: ts(),
        };

        let encoded = serde_json::to_string(&update).unwrap();
        let decoded: MarketUpdate = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, update);
    }

    #[test]
    fn new_sports_variants_round_trip() {
        let pregame = MarketUpdate::SportsPregame {
            game_id: Arc::from("game"),
            league: Arc::from("nba"),
            home_team: Arc::from("Home"),
            away_team: Arc::from("Away"),
            start_time: ts(),
            home_odds: 0.52,
            away_odds: 0.48,
            model_home_prob: Some(0.55),
            ts: ts(),
        };
        let live = MarketUpdate::SportsLive {
            game_id: Arc::from("game"),
            league: Arc::from("nba"),
            period: Arc::from("Q4"),
            home_score: 101,
            away_score: 99,
            clock_remaining_secs: Some(30),
            momentum: 0.15,
            ts: ts(),
        };

        for update in [pregame, live] {
            let encoded = serde_json::to_string(&update).unwrap();
            let decoded: MarketUpdate = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, update);
        }
    }

    #[test]
    fn contract_enums_parse_snake_case() {
        let family: PredictionFamily = serde_json::from_str("\"crypto_expiry\"").unwrap();
        let instrument: InstrumentKind = serde_json::from_str("\"up_down\"").unwrap();
        let venue: VenueKind = serde_json::from_str("\"polymarket\"").unwrap();

        assert_eq!(family, PredictionFamily::CryptoExpiry);
        assert_eq!(instrument, InstrumentKind::UpDown);
        assert_eq!(venue, VenueKind::Polymarket);
    }

    #[test]
    fn normalize_token_id_converts_large_hex() {
        let raw = "\"0x3c38c18444ab803acea0d4de7bcdecae7f0f8ddbcd0466e3323d1cb9e04b6f5d\"";
        assert_eq!(
            normalize_token_id(raw),
            "27239049953613250678046988034203198692578441444398010699401021233149338414941"
        );
    }

    #[test]
    fn normalize_token_id_keeps_decimal_ids() {
        let raw = "12345678901234567890";
        assert_eq!(normalize_token_id(raw), raw);
    }

    #[test]
    fn event_discovered_sort_ts_uses_window_and_buffer() {
        let end_time = ts();
        let update = MarketUpdate::EventDiscovered {
            event_id: Arc::from("evt"),
            symbol: Arc::from("BTCUSDT"),
            up_token: Arc::from("up"),
            down_token: Arc::from("down"),
            end_time,
            window_secs: 300,
            price_to_beat: None,
            resolved_up_won: None,
        };

        assert_eq!(
            market_update_sort_ts(&update),
            end_time - chrono::Duration::seconds(300) - chrono::Duration::hours(1)
        );
    }

    #[test]
    fn legacy_reference_price_without_received_at_still_deserializes() {
        let json = r#"{
            "kind":"reference_price",
            "symbol":"btc/usd",
            "source":"chainlink",
            "asset_class":"crypto",
            "price":"100000",
            "full_accuracy_value":null,
            "is_carried_forward":false,
            "ts":"2026-04-21T00:00:00Z"
        }"#;

        let update: MarketUpdate = serde_json::from_str(json).unwrap();
        assert!(matches!(
            update,
            MarketUpdate::ReferencePrice {
                received_at: None,
                ..
            }
        ));
    }

    #[test]
    fn reference_price_sort_ts_uses_received_at() {
        let source_ts = ts();
        let received_at = source_ts + chrono::Duration::seconds(3);
        let update = MarketUpdate::ReferencePrice {
            symbol: Arc::from("btc/usd"),
            source: Arc::from("chainlink"),
            asset_class: Arc::from("crypto"),
            price: Decimal::new(100_000, 0),
            full_accuracy_value: None,
            is_carried_forward: false,
            received_at: Some(received_at),
            ts: source_ts,
        };

        assert_eq!(market_update_sort_ts(&update), received_at);
    }
}
