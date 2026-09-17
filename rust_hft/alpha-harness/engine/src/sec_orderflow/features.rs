use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use hft_research_manifest::sec_orderflow::SecOrderflowError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TradeSourceRef {
    pub path: String,
    pub line: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowTradeFragment {
    pub ts: i64,
    pub sec: i64,
    #[serde(default)]
    pub symbol: Option<String>,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    #[serde(rename = "buyVol")]
    pub buy_vol: f64,
    #[serde(rename = "sellVol")]
    pub sell_vol: f64,
    #[serde(rename = "buyTo")]
    pub buy_to: f64,
    #[serde(rename = "sellTo")]
    pub sell_to: f64,
    #[serde(rename = "buyN")]
    pub buy_n: u64,
    #[serde(rename = "sellN")]
    pub sell_n: u64,
    #[serde(rename = "tradesN")]
    pub trades_n: u64,
    #[serde(rename = "tradesTo")]
    pub trades_to: f64,
    pub vwap: f64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub source: TradeSourceRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowBookSnapshot {
    pub ts: i64,
    pub symbol: String,
    pub mid: f64,
    pub bid1: f64,
    pub ask1: f64,
    #[serde(rename = "spreadBp")]
    pub spread_bp: f64,
    #[serde(rename = "bidNotional5")]
    pub bid_notional5: f64,
    #[serde(rename = "askNotional5")]
    pub ask_notional5: f64,
    #[serde(rename = "imb5")]
    pub source_imb5: f64,
    #[serde(rename = "imb20")]
    pub source_imb20: f64,
    pub bids: Vec<[f64; 2]>,
    pub asks: Vec<[f64; 2]>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub source: TradeSourceRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergedTradeSecond {
    pub sec: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub buy_to: f64,
    pub sell_to: f64,
    pub buy_n: u64,
    pub sell_n: u64,
    pub trades_n: u64,
    pub trades_to: f64,
    pub signed_notional: f64,
    pub historical_cvd_discarded: bool,
    pub unknown_source_fields: Vec<String>,
    pub sources: Vec<TradeSourceRef>,
}

impl MergedTradeSecond {
    pub fn available_time_ms(&self) -> i64 {
        self.sec
            .checked_add(1)
            .and_then(|sec| sec.checked_mul(1_000))
            .unwrap_or(i64::MAX)
    }
}

pub fn merge_trade_fragments(
    fragments: &[FlowTradeFragment],
) -> Result<MergedTradeSecond, SecOrderflowError> {
    let first = fragments
        .first()
        .ok_or(SecOrderflowError::Invalid("trade second has no fragments"))?;
    if fragments.iter().any(|fragment| fragment.sec != first.sec) {
        return Err(SecOrderflowError::Invalid("cannot merge distinct seconds"));
    }
    if fragments.iter().any(|fragment| {
        !fragment.o.is_finite()
            || !fragment.h.is_finite()
            || !fragment.l.is_finite()
            || !fragment.c.is_finite()
            || fragment.h < fragment.l
            || fragment.h < fragment.o
            || fragment.h < fragment.c
            || fragment.l > fragment.o
            || fragment.l > fragment.c
    }) {
        return Err(SecOrderflowError::Invalid("invalid OHLC fragment"));
    }
    let mut unknown = BTreeSet::new();
    let mut merged = MergedTradeSecond {
        sec: first.sec,
        open: first.o,
        high: first.h,
        low: first.l,
        close: first.c,
        buy_vol: 0.0,
        sell_vol: 0.0,
        buy_to: 0.0,
        sell_to: 0.0,
        buy_n: 0,
        sell_n: 0,
        trades_n: 0,
        trades_to: 0.0,
        signed_notional: 0.0,
        historical_cvd_discarded: true,
        unknown_source_fields: Vec::new(),
        sources: Vec::new(),
    };
    for fragment in fragments {
        merged.high = merged.high.max(fragment.h);
        merged.low = merged.low.min(fragment.l);
        merged.close = fragment.c;
        merged.buy_vol += fragment.buy_vol;
        merged.sell_vol += fragment.sell_vol;
        merged.buy_to += fragment.buy_to;
        merged.sell_to += fragment.sell_to;
        merged.buy_n += fragment.buy_n;
        merged.sell_n += fragment.sell_n;
        merged.trades_n += fragment.trades_n;
        merged.trades_to += fragment.trades_to;
        merged.sources.push(fragment.source.clone());
        unknown.extend(fragment.extra.keys().cloned());
    }
    merged.signed_notional = merged.buy_to - merged.sell_to;
    merged.unknown_source_fields = unknown.into_iter().collect();
    Ok(merged)
}

pub fn as_of_book(books: &[FlowBookSnapshot], available_time_ms: i64) -> Option<&FlowBookSnapshot> {
    books
        .iter()
        .filter(|book| book.ts < available_time_ms)
        .max_by_key(|book| book.ts)
}

pub fn qty_imbalance(levels: &[[f64; 2]], other: &[[f64; 2]]) -> Option<f64> {
    let bid: f64 = levels.iter().map(|level| level[1]).sum();
    let ask: f64 = other.iter().map(|level| level[1]).sum();
    let denom = bid + ask;
    if denom > 0.0 && denom.is_finite() {
        Some((bid - ask) / denom)
    } else {
        None
    }
}

pub fn book_crossed(book: &FlowBookSnapshot) -> bool {
    book.bid1 >= book.ask1 || !book.mid.is_finite() || book.mid <= 0.0
}

pub fn last_age_ms(trade: &MergedTradeSecond, available_time_ms: i64) -> i64 {
    available_time_ms.saturating_sub(trade.sec.saturating_mul(1_000))
}
