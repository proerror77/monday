use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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
    pub const OKX: VenueId = VenueId(4);
    pub const HYPERLIQUID: VenueId = VenueId(5);
    pub const ASTERDEX: VenueId = VenueId(6);
    pub const LIGHTER: VenueId = VenueId(7);
    pub const BACKPACK: VenueId = VenueId(8);
    pub const GRVT: VenueId = VenueId(9);
    pub const MOCK: VenueId = VenueId(99);

    pub fn as_str(&self) -> &'static str {
        match *self {
            Self::BINANCE => "BINANCE",
            Self::BITGET => "BITGET",
            Self::BYBIT => "BYBIT",
            Self::OKX => "OKX",
            Self::HYPERLIQUID => "HYPERLIQUID",
            Self::ASTERDEX => "ASTERDEX",
            Self::LIGHTER => "LIGHTER",
            Self::BACKPACK => "BACKPACK",
            Self::GRVT => "GRVT",
            Self::MOCK => "MOCK",
            _ => "UNKNOWN",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "BINANCE" => Some(Self::BINANCE),
            "BITGET" => Some(Self::BITGET),
            "BYBIT" => Some(Self::BYBIT),
            "OKX" => Some(Self::OKX),
            "HYPERLIQUID" => Some(Self::HYPERLIQUID),
            "ASTERDEX" => Some(Self::ASTERDEX),
            "LIGHTER" => Some(Self::LIGHTER),
            "BACKPACK" => Some(Self::BACKPACK),
            "GRVT" => Some(Self::GRVT),
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

/// 帳戶標識符（Phase 1：字串型，便於與外部系統對齊）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId(pub String);

impl From<&str> for AccountId {
    fn from(s: &str) -> Self {
        AccountId(s.to_string())
    }
}
impl From<String> for AccountId {
    fn from(s: String) -> Self {
        AccountId(s)
    }
}
impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
        let value = Decimal::from_str(s)?;
        if value <= Decimal::ZERO {
            return Err(rust_decimal::Error::ConversionTo("Price".into()));
        }
        Ok(Price(value))
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
        let value = Decimal::from_str(s)?;
        if value <= Decimal::ZERO {
            return Err(rust_decimal::Error::ConversionTo("Quantity".into()));
        }
        Ok(Quantity(value))
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

/// 場所+符號 組合鍵（用於保存 per-venue 訂單簿）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VenueSymbol {
    pub venue: VenueId,
    pub symbol: Symbol,
}

impl VenueSymbol {
    pub fn new(venue: VenueId, symbol: Symbol) -> Self {
        Self { venue, symbol }
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
pub enum Side {
    Buy,
    Sell,
}

/// 與舊版 API 相容的別名，允許現有程式碼繼續使用 `OrderSide`。
pub type OrderSide = Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    GTC,
    IOC,
    FOK,
}

// （重複定義移除）
