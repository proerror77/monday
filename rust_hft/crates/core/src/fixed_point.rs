//! 高性能定点数运算
//! 
//! 为热路径提供快速的定点数算术，精度固定为6位小数
//! 内部使用 i64 表示，避免 Decimal 的堆分配和复杂运算

use serde::{Deserialize, Serialize};

/// 固定精度 (6位小数): 1.123456 -> 1123456
const SCALE: i64 = 1_000_000;

/// 高性能定点价格类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FixedPrice(i64);

impl FixedPrice {
    /// 零值常量
    pub const ZERO: FixedPrice = FixedPrice(0);
    
    /// 从 f64 创建 (内联，热路径优化)
    #[inline]
    pub fn from_f64(value: f64) -> Self {
        FixedPrice((value * SCALE as f64).round() as i64)
    }
    
    /// 从字符串创建 (非热路径)
    pub fn from_str(s: &str) -> Result<Self, std::num::ParseFloatError> {
        let value: f64 = s.parse()?;
        Ok(Self::from_f64(value))
    }
    
    /// 转为 f64 (用于边界输出)
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / SCALE as f64
    }
    
    /// 快速加法 (内联，热路径)
    #[inline]
    pub fn add(self, other: FixedPrice) -> FixedPrice {
        FixedPrice(self.0 + other.0)
    }
    
    /// 快速减法 (内联，热路径)
    #[inline]
    pub fn sub(self, other: FixedPrice) -> FixedPrice {
        FixedPrice(self.0 - other.0)
    }
    
    /// 快速乘法 (内联，热路径)
    #[inline]
    pub fn mul(self, other: FixedPrice) -> FixedPrice {
        FixedPrice((self.0 * other.0) / SCALE)
    }
    
    /// 快速除法 (内联，热路径)
    #[inline]
    pub fn div(self, other: FixedPrice) -> FixedPrice {
        FixedPrice((self.0 * SCALE) / other.0)
    }
    
    /// 快速中间价计算 (热路径优化)
    #[inline]
    pub fn mid(bid: FixedPrice, ask: FixedPrice) -> FixedPrice {
        FixedPrice((bid.0 + ask.0) / 2)
    }
    
    /// 快速价差基点计算 (热路径优化)
    #[inline]
    pub fn spread_bps(bid: FixedPrice, ask: FixedPrice) -> FixedBps {
        let spread = ask.0 - bid.0;
        let mid = (bid.0 + ask.0) / 2;
        FixedBps((spread * 10000 * SCALE) / mid)
    }
    
    /// 原始值访问 (内部优化使用)
    #[inline]
    pub fn raw(self) -> i64 {
        self.0
    }
}

/// 高性能定点数量类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FixedQuantity(i64);

impl FixedQuantity {
    pub const ZERO: FixedQuantity = FixedQuantity(0);
    
    #[inline]
    pub fn from_f64(value: f64) -> Self {
        FixedQuantity((value * SCALE as f64).round() as i64)
    }
    
    pub fn from_str(s: &str) -> Result<Self, std::num::ParseFloatError> {
        let value: f64 = s.parse()?;
        Ok(Self::from_f64(value))
    }
    
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / SCALE as f64
    }
    
    #[inline]
    pub fn add(self, other: FixedQuantity) -> FixedQuantity {
        FixedQuantity(self.0 + other.0)
    }
    
    #[inline]
    pub fn sub(self, other: FixedQuantity) -> FixedQuantity {
        FixedQuantity(self.0 - other.0)
    }
    
    #[inline]
    pub fn raw(self) -> i64 {
        self.0
    }
}

/// 高性能定点基点类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FixedBps(i64);

impl FixedBps {
    pub const ZERO: FixedBps = FixedBps(0);
    
    #[inline]
    pub fn from_f64(value: f64) -> Self {
        FixedBps((value * SCALE as f64).round() as i64)
    }
    
    pub fn from_str(s: &str) -> Result<Self, std::num::ParseFloatError> {
        let value: f64 = s.parse()?;
        Ok(Self::from_f64(value))
    }
    
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / SCALE as f64
    }
    
    #[inline]
    pub fn raw(self) -> i64 {
        self.0
    }
}

/// 与原有 Decimal 类型的转换函数 (边界使用)
impl From<crate::Price> for FixedPrice {
    fn from(price: crate::Price) -> Self {
        FixedPrice::from_f64(price.to_f64().unwrap_or(0.0))
    }
}

impl From<FixedPrice> for crate::Price {
    fn from(fixed: FixedPrice) -> Self {
        crate::Price::from_f64(fixed.to_f64()).unwrap_or(crate::Price::zero())
    }
}

impl From<crate::Quantity> for FixedQuantity {
    fn from(qty: crate::Quantity) -> Self {
        FixedQuantity::from_f64(qty.to_f64().unwrap_or(0.0))
    }
}

impl From<FixedQuantity> for crate::Quantity {
    fn from(fixed: FixedQuantity) -> Self {
        crate::Quantity::from_f64(fixed.to_f64()).unwrap_or(crate::Quantity::zero())
    }
}

impl From<crate::Bps> for FixedBps {
    fn from(bps: crate::Bps) -> Self {
        FixedBps::from_f64(bps.to_f64().unwrap_or(0.0))
    }
}

impl From<FixedBps> for crate::Bps {
    fn from(fixed: FixedBps) -> Self {
        crate::Bps::from_f64(fixed.to_f64()).unwrap_or(crate::Bps::zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_price_basic() {
        let p1 = FixedPrice::from_f64(123.456);
        let p2 = FixedPrice::from_f64(678.123);
        
        assert!((p1.to_f64() - 123.456).abs() < 0.000001);
        assert!((p2.to_f64() - 678.123).abs() < 0.000001);
    }
    
    #[test]
    fn test_fixed_price_arithmetic() {
        let p1 = FixedPrice::from_f64(100.0);
        let p2 = FixedPrice::from_f64(50.0);
        
        let sum = p1.add(p2);
        let diff = p1.sub(p2);
        let mid = FixedPrice::mid(p1, p2);
        
        assert!((sum.to_f64() - 150.0).abs() < 0.000001);
        assert!((diff.to_f64() - 50.0).abs() < 0.000001);
        assert!((mid.to_f64() - 75.0).abs() < 0.000001);
    }
    
    #[test]
    fn test_spread_bps() {
        let bid = FixedPrice::from_f64(100.0);
        let ask = FixedPrice::from_f64(100.1);
        
        let spread = FixedPrice::spread_bps(bid, ask);
        
        // 0.1 / 100.05 * 10000 ≈ 9.995 bps
        assert!((spread.to_f64() - 9.995).abs() < 0.01);
    }
}