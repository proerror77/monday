use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::MarketUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinanceSourceKind {
    Spot,
    AggTrade,
    L2,
}

/// Exchange/source time paired with the time a historical row became visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinanceSourceClock {
    pub kind: BinanceSourceKind,
    pub symbol: String,
    pub source_ts: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_id: Option<u64>,
}

/// Additive historical-loader flags for non-crypto datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalLoadOptions {
    pub include_reference_prices: bool,
    pub reference_symbols: Vec<String>,
    pub include_sports_state: bool,
    pub require_official_settlement: bool,
    /// Include `binance_lob_ticks` as generic L2 market updates.
    ///
    /// Research jobs that load richer LOB snapshots separately can disable this
    /// to avoid scanning the same large table twice.
    pub include_l2: bool,
    /// Downsample `binance_lob_ticks` to one snapshot per N seconds per symbol.
    /// Defaults to 30 (one row per 30-second bucket). Set to 1 to disable downsampling.
    pub lob_sample_secs: u32,
    /// Downsample high-frequency spot price ticks to one row per N seconds per symbol.
    ///
    /// Factor review jobs use coarser observation buckets and should not load
    /// every `sync_records` tick into memory. Defaults to 1 second.
    pub spot_sample_secs: u32,
    /// Reject captured CEX rows whose arrival lags exchange time by more than N seconds.
    pub max_source_delay_secs: u32,
}

impl Default for HistoricalLoadOptions {
    fn default() -> Self {
        Self {
            include_reference_prices: false,
            reference_symbols: Vec::new(),
            include_sports_state: false,
            require_official_settlement: false,
            include_l2: true,
            lob_sample_secs: 30,
            spot_sample_secs: 1,
            max_source_delay_secs: 30,
        }
    }
}

impl HistoricalLoadOptions {
    #[must_use]
    pub fn normalized_reference_symbols(&self) -> Vec<String> {
        self.reference_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}

/// Data feed source: historical replay, recording replay, or live stream.
#[async_trait]
pub trait Feed: Send {
    async fn next(&mut self) -> Option<MarketUpdate>;
}

#[async_trait]
impl<T> Feed for Box<T>
where
    T: Feed + ?Sized,
{
    async fn next(&mut self) -> Option<MarketUpdate> {
        (**self).next().await
    }
}
