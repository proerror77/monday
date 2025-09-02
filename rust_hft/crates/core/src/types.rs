use serde::{Deserialize, Serialize};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::str::FromStr;
use std::fmt;

// 時間戳（微秒）
pub type Timestamp = u64;

// === 顯式 Venue 語義類型（Phase 1 重構）===

/// 交易所/場所標識符（數值型，避免字串分配）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VenueId(pub u16);

impl VenueId {
    pub const BINANCE: VenueId = VenueId(1);
    pub const BITGET: VenueId = VenueId(2);
    pub const BYBIT: VenueId = VenueId(3);
    pub const MOCK: VenueId = VenueId(99);
    
    pub fn as_str(&self) -> &'static str {
        match *self {
            Self::BINANCE => "BINANCE",
            Self::BITGET => "BITGET", 
            Self::BYBIT => "BYBIT",
            Self::MOCK => "MOCK",
            _ => "UNKNOWN",
        }
    }
    
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "BINANCE" => Some(Self::BINANCE),
            "BITGET" => Some(Self::BITGET),
            "BYBIT" => Some(Self::BYBIT),
            "MOCK" => Some(Self::MOCK),
            _ => None,
        }
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 基礎交易對符號（不含場所前綴，用於跨所聚合）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseSymbol(pub String);

impl BaseSymbol {
    pub fn new(symbol: impl Into<String>) -> Self {
        BaseSymbol(symbol.into())
    }
    
    /// 從帶場所前綴的 symbol 提取基礎符號
    /// 例如: "BINANCE:BTCUSDT" -> "BTCUSDT"
    pub fn from_venue_symbol(venue_symbol: &str) -> Self {
        if let Some(colon_pos) = venue_symbol.find(':') {
            BaseSymbol(venue_symbol[colon_pos + 1..].to_string())
        } else {
            BaseSymbol(venue_symbol.to_string())
        }
    }
    
    /// 構造帶場所前綴的完整符號
    pub fn to_venue_symbol(&self, venue: VenueId) -> String {
        format!("{}:{}", venue, self.0)
    }
}

impl fmt::Display for BaseSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for BaseSymbol {
    fn from(s: &str) -> Self {
        BaseSymbol::from_venue_symbol(s)
    }
}

impl From<String> for BaseSymbol {
    fn from(s: String) -> Self {
        BaseSymbol::from_venue_symbol(&s)
    }
}

// 注意：UnifiedTimestamp 已遷移至 `unified_timestamp` 模組，
// 並由 crate root 透過 `pub use unified_timestamp::UnifiedTimestamp` 導出。
// 請使用 `hft_core::UnifiedTimestamp`。

// 價格/數量新型別（精確定點數學）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Price(pub Decimal);

impl Price {
    pub fn from_f64(v: f64) -> Result<Self, rust_decimal::Error> {
        Ok(Price(Decimal::try_from(v)?))
    }
    
    pub fn from_str(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Price(Decimal::from_str(s)?))
    }
    
    pub fn zero() -> Self {
        Price(Decimal::ZERO)
    }
    
    pub fn to_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }
}

impl std::fmt::Display for Price {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Quantity(pub Decimal);

impl Quantity {
    pub fn from_f64(v: f64) -> Result<Self, rust_decimal::Error> {
        Ok(Quantity(Decimal::try_from(v)?))
    }
    
    pub fn from_str(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Quantity(Decimal::from_str(s)?))
    }
    
    pub fn zero() -> Self {
        Quantity(Decimal::ZERO)
    }
    
    pub fn to_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }
}

impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// 符號/ID/Bps
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Symbol(pub String);

impl Default for Symbol {
    fn default() -> Self {
        Symbol("".to_string())
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Bps(pub Decimal);

impl Bps {
    pub fn from_f64(v: f64) -> Result<Self, rust_decimal::Error> {
        Ok(Bps(Decimal::try_from(v)?))
    }
    
    pub fn from_str(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Bps(Decimal::from_str(s)?))
    }
    
    pub fn zero() -> Self {
        Bps(Decimal::ZERO)
    }
    
    pub fn to_f64(&self) -> Option<f64> {
        self.0.to_f64()
    }
}

// 市場方向/訂單型別/時效
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Buy, Sell }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType { Market, Limit }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce { GTC, IOC, FOK }

// （重複定義移除）
