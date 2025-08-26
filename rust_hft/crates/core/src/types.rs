use serde::{Deserialize, Serialize};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::str::FromStr;

// 時間戳（微秒）
pub type Timestamp = u64;

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
