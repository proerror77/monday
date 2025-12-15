//! TopN OrderBook - 專為 HFT 優化的 SoA (Structure of Arrays) 訂單簿實現
//!
//! 特點：
//! - 連續內存佈局，支持 SIMD 向量化操作
//! - 固定大小的 TopN 結構，避免動態分配
//! - 零拷貝價格計算和套利檢測
//! - 支持多交易所價格對比

use crate::error::HftError;
use crate::types::{Symbol, Timestamp};
use std::fmt;

/// TopN 訂單簿配置
pub const DEFAULT_TOP_N: usize = 10;
pub const MAX_TOP_N: usize = 20;

/// TopN 訂單簿 - Structure of Arrays (SoA) 佈局
///
/// 相比傳統的 BTreeMap<Price, Quantity>，此結構提供：
/// 1. 連續內存佈局，提升 CPU 緩存效率
/// 2. SIMD 友好的數據排列，支持向量化運算
/// 3. 固定大小數組，避免動態分配開銷
/// 4. 快速的 TopN 套利檢測
#[derive(Debug, Clone)]
pub struct TopNOrderBook<const N: usize = DEFAULT_TOP_N> {
    pub symbol: Symbol,
    pub last_update_ts: Timestamp,

    // Bids (買盤) - 按價格從高到低排序
    pub bid_prices: [f64; N], // 連續內存，SIMD 友好
    pub bid_quantities: [f64; N],
    pub bid_count: usize, // 實際有效 bid 數量

    // Asks (賣盤) - 按價格從低到高排序
    pub ask_prices: [f64; N], // 連續內存，SIMD 友好
    pub ask_quantities: [f64; N],
    pub ask_count: usize, // 實際有效 ask 數量

    // 快照版本號，用於檢測更新
    pub version: u64,
}

impl<const N: usize> Default for TopNOrderBook<N> {
    fn default() -> Self {
        Self {
            symbol: Symbol::default(),
            last_update_ts: 0,
            bid_prices: [0.0; N],
            bid_quantities: [0.0; N],
            bid_count: 0,
            ask_prices: [0.0; N],
            ask_quantities: [0.0; N],
            ask_count: 0,
            version: 0,
        }
    }
}

impl<const N: usize> TopNOrderBook<N> {
    pub fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            ..Default::default()
        }
    }

    /// 應用完整快照（清空後重建）
    pub fn apply_snapshot(
        &mut self,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        timestamp: Timestamp,
    ) -> Result<(), HftError> {
        self.clear();
        self.last_update_ts = timestamp;
        self.version = self.version.wrapping_add(1);

        // 插入 bids（假設已按價格從高到低排序）
        for (i, &(price, qty)) in bids.iter().take(N).enumerate() {
            if qty > 0.0 {
                self.bid_prices[i] = price;
                self.bid_quantities[i] = qty;
                self.bid_count = i + 1;
            }
        }

        // 插入 asks（假設已按價格從低到高排序）
        for (i, &(price, qty)) in asks.iter().take(N).enumerate() {
            if qty > 0.0 {
                self.ask_prices[i] = price;
                self.ask_quantities[i] = qty;
                self.ask_count = i + 1;
            }
        }

        Ok(())
    }

    /// 應用增量更新（修改特定價格檔位）
    pub fn apply_update(
        &mut self,
        bid_updates: &[(f64, f64)],
        ask_updates: &[(f64, f64)],
        timestamp: Timestamp,
    ) -> Result<(), HftError> {
        self.last_update_ts = timestamp;
        self.version = self.version.wrapping_add(1);

        // 處理 bid 更新
        for &(price, qty) in bid_updates {
            self.update_bid_level(price, qty)?;
        }

        // 處理 ask 更新
        for &(price, qty) in ask_updates {
            self.update_ask_level(price, qty)?;
        }

        Ok(())
    }

    /// 更新單個 bid 價格檔位
    fn update_bid_level(&mut self, price: f64, qty: f64) -> Result<(), HftError> {
        // 查找是否已存在該價格
        for i in 0..self.bid_count {
            if (self.bid_prices[i] - price).abs() < f64::EPSILON {
                if qty <= 0.0 {
                    // 刪除該檔位
                    self.remove_bid_at_index(i);
                } else {
                    // 更新數量
                    self.bid_quantities[i] = qty;
                }
                return Ok(());
            }
        }

        // 新價格，需要插入
        if qty > 0.0 {
            self.insert_bid_level(price, qty)?;
        }

        Ok(())
    }

    /// 更新單個 ask 價格檔位
    fn update_ask_level(&mut self, price: f64, qty: f64) -> Result<(), HftError> {
        // 查找是否已存在該價格
        for i in 0..self.ask_count {
            if (self.ask_prices[i] - price).abs() < f64::EPSILON {
                if qty <= 0.0 {
                    // 刪除該檔位
                    self.remove_ask_at_index(i);
                } else {
                    // 更新數量
                    self.ask_quantities[i] = qty;
                }
                return Ok(());
            }
        }

        // 新價格，需要插入
        if qty > 0.0 {
            self.insert_ask_level(price, qty)?;
        }

        Ok(())
    }

    /// 插入新的 bid 檔位（保持從高到低排序）
    fn insert_bid_level(&mut self, price: f64, qty: f64) -> Result<(), HftError> {
        if self.bid_count >= N {
            // 檢查新價格是否比最低 bid 更高
            if price <= self.bid_prices[N - 1] {
                return Ok(()); // 新價格太低，忽略
            }
            self.bid_count = N - 1; // 為新檔位騰出空間
        }

        // 找到插入位置（保持從高到低順序）
        let mut insert_pos = self.bid_count;
        for i in 0..self.bid_count {
            if price > self.bid_prices[i] {
                insert_pos = i;
                break;
            }
        }

        // 向後移動元素為新檔位騰出空間
        for i in (insert_pos..self.bid_count).rev() {
            self.bid_prices[i + 1] = self.bid_prices[i];
            self.bid_quantities[i + 1] = self.bid_quantities[i];
        }

        // 插入新檔位
        self.bid_prices[insert_pos] = price;
        self.bid_quantities[insert_pos] = qty;
        self.bid_count = (self.bid_count + 1).min(N);

        Ok(())
    }

    /// 插入新的 ask 檔位（保持從低到高排序）
    fn insert_ask_level(&mut self, price: f64, qty: f64) -> Result<(), HftError> {
        if self.ask_count >= N {
            // 檢查新價格是否比最高 ask 更低
            if price >= self.ask_prices[N - 1] {
                return Ok(()); // 新價格太高，忽略
            }
            self.ask_count = N - 1; // 為新檔位騰出空間
        }

        // 找到插入位置（保持從低到高順序）
        let mut insert_pos = self.ask_count;
        for i in 0..self.ask_count {
            if price < self.ask_prices[i] {
                insert_pos = i;
                break;
            }
        }

        // 向後移動元素為新檔位騰出空間
        for i in (insert_pos..self.ask_count).rev() {
            self.ask_prices[i + 1] = self.ask_prices[i];
            self.ask_quantities[i + 1] = self.ask_quantities[i];
        }

        // 插入新檔位
        self.ask_prices[insert_pos] = price;
        self.ask_quantities[insert_pos] = qty;
        self.ask_count = (self.ask_count + 1).min(N);

        Ok(())
    }

    /// 刪除指定索引的 bid 檔位
    fn remove_bid_at_index(&mut self, index: usize) {
        if index < self.bid_count {
            // 向前移動後續元素
            for i in index..(self.bid_count - 1) {
                self.bid_prices[i] = self.bid_prices[i + 1];
                self.bid_quantities[i] = self.bid_quantities[i + 1];
            }
            self.bid_count -= 1;
        }
    }

    /// 刪除指定索引的 ask 檔位
    fn remove_ask_at_index(&mut self, index: usize) {
        if index < self.ask_count {
            // 向前移動後續元素
            for i in index..(self.ask_count - 1) {
                self.ask_prices[i] = self.ask_prices[i + 1];
                self.ask_quantities[i] = self.ask_quantities[i + 1];
            }
            self.ask_count -= 1;
        }
    }

    /// 清空訂單簿
    pub fn clear(&mut self) {
        self.bid_count = 0;
        self.ask_count = 0;
        self.bid_prices.fill(0.0);
        self.bid_quantities.fill(0.0);
        self.ask_prices.fill(0.0);
        self.ask_quantities.fill(0.0);
    }

    /// 獲取最佳買價（bid[0]）
    pub fn best_bid(&self) -> Option<f64> {
        if self.bid_count > 0 {
            Some(self.bid_prices[0])
        } else {
            None
        }
    }

    /// 獲取最佳賣價（ask[0]）
    pub fn best_ask(&self) -> Option<f64> {
        if self.ask_count > 0 {
            Some(self.ask_prices[0])
        } else {
            None
        }
    }

    /// 計算價差（ask[0] - bid[0]）
    pub fn spread(&self) -> Option<f64> {
        match (self.best_ask(), self.best_bid()) {
            (Some(ask), Some(bid)) => Some(ask - bid),
            _ => None,
        }
    }

    /// 計算中間價格（(ask[0] + bid[0]) / 2）
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_ask(), self.best_bid()) {
            (Some(ask), Some(bid)) => Some((ask + bid) * 0.5),
            _ => None,
        }
    }

    /// 檢查訂單簿是否有效（至少有一邊報價）
    pub fn is_valid(&self) -> bool {
        self.bid_count > 0 || self.ask_count > 0
    }

    /// 獲取指定深度的 bid 檔位
    pub fn bid_level(&self, depth: usize) -> Option<(f64, f64)> {
        if depth < self.bid_count {
            Some((self.bid_prices[depth], self.bid_quantities[depth]))
        } else {
            None
        }
    }

    /// 獲取指定深度的 ask 檔位
    pub fn ask_level(&self, depth: usize) -> Option<(f64, f64)> {
        if depth < self.ask_count {
            Some((self.ask_prices[depth], self.ask_quantities[depth]))
        } else {
            None
        }
    }

    /// 計算指定數量的最佳執行價格（VWAP）
    pub fn vwap_bid(&self, target_qty: f64) -> Option<f64> {
        let mut remaining_qty = target_qty;
        let mut weighted_sum = 0.0;
        let mut total_qty = 0.0;

        for i in 0..self.bid_count {
            let level_qty = self.bid_quantities[i].min(remaining_qty);
            weighted_sum += self.bid_prices[i] * level_qty;
            total_qty += level_qty;
            remaining_qty -= level_qty;

            if remaining_qty <= f64::EPSILON {
                break;
            }
        }

        if total_qty > f64::EPSILON {
            Some(weighted_sum / total_qty)
        } else {
            None
        }
    }

    /// 計算指定數量的最佳執行價格（VWAP）
    pub fn vwap_ask(&self, target_qty: f64) -> Option<f64> {
        let mut remaining_qty = target_qty;
        let mut weighted_sum = 0.0;
        let mut total_qty = 0.0;

        for i in 0..self.ask_count {
            let level_qty = self.ask_quantities[i].min(remaining_qty);
            weighted_sum += self.ask_prices[i] * level_qty;
            total_qty += level_qty;
            remaining_qty -= level_qty;

            if remaining_qty <= f64::EPSILON {
                break;
            }
        }

        if total_qty > f64::EPSILON {
            Some(weighted_sum / total_qty)
        } else {
            None
        }
    }
}

impl<const N: usize> fmt::Display for TopNOrderBook<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "TopN{} OrderBook [{}] (v{})",
            N, self.symbol, self.version
        )?;
        writeln!(f, "Timestamp: {}", self.last_update_ts)?;

        // 顯示 asks（從高到低，方便視覺對齊）
        for i in (0..self.ask_count).rev() {
            writeln!(
                f,
                "  Ask[{}]: {:>12.4} @ {:>10.4}",
                i, self.ask_quantities[i], self.ask_prices[i]
            )?;
        }

        // 分隔線
        if self.ask_count > 0 && self.bid_count > 0 {
            if let Some(spread) = self.spread() {
                writeln!(f, "  -------- Spread: {:>6.4} --------", spread)?;
            }
        }

        // 顯示 bids（從高到低）
        for i in 0..self.bid_count {
            writeln!(
                f,
                "  Bid[{}]: {:>12.4} @ {:>10.4}",
                i, self.bid_quantities[i], self.bid_prices[i]
            )?;
        }

        Ok(())
    }
}

/// 雙邊 TopN 訂單簿 Joiner，用於跨交易所套利檢測
#[derive(Debug, Clone)]
pub struct DualTopNJoiner<const N: usize = DEFAULT_TOP_N> {
    pub venue_a: TopNOrderBook<N>,
    pub venue_b: TopNOrderBook<N>,
    pub min_spread_bps: f64, // 最小套利價差（基點）
    pub max_stale_us: u64,   // 最大陳舊時間（微秒）
}

impl<const N: usize> DualTopNJoiner<N> {
    pub fn new(symbol_a: Symbol, symbol_b: Symbol, min_spread_bps: f64) -> Self {
        Self {
            venue_a: TopNOrderBook::new(symbol_a),
            venue_b: TopNOrderBook::new(symbol_b),
            min_spread_bps,
            max_stale_us: 3000, // 3ms 陳舊度限制
        }
    }

    /// 檢查跨交易所套利機會
    /// 返回：(venue_buy, venue_sell, expected_profit_bps)
    pub fn check_arbitrage(&self, current_ts: Timestamp) -> Option<(bool, f64)> {
        // 檢查數據陳舊度
        if current_ts.saturating_sub(self.venue_a.last_update_ts) > self.max_stale_us
            || current_ts.saturating_sub(self.venue_b.last_update_ts) > self.max_stale_us
        {
            return None;
        }

        let bid_a = self.venue_a.best_bid()?;
        let ask_a = self.venue_a.best_ask()?;
        let bid_b = self.venue_b.best_bid()?;
        let ask_b = self.venue_b.best_ask()?;

        // A 買 B 賣：A.ask < B.bid
        let arb_a_buy_b_sell = (bid_b - ask_a) / ask_a * 10000.0;

        // B 買 A 賣：B.ask < A.bid
        let arb_b_buy_a_sell = (bid_a - ask_b) / ask_b * 10000.0;

        if arb_a_buy_b_sell > self.min_spread_bps {
            Some((true, arb_a_buy_b_sell)) // A 買 B 賣
        } else if arb_b_buy_a_sell > self.min_spread_bps {
            Some((false, arb_b_buy_a_sell)) // B 買 A 賣
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topn_orderbook_basic() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        // 測試應用快照
        let bids = vec![(67200.0, 1.5), (67199.0, 2.0), (67198.0, 1.0)];
        let asks = vec![(67201.0, 1.2), (67202.0, 0.8), (67203.0, 2.5)];

        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        assert_eq!(book.best_bid(), Some(67200.0));
        assert_eq!(book.best_ask(), Some(67201.0));
        assert_eq!(book.spread(), Some(1.0));
        assert_eq!(book.mid_price(), Some(67200.5));
    }

    #[test]
    fn test_topn_orderbook_updates() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("ETHUSDT"));

        // 初始快照
        let bids = vec![(3500.0, 10.0), (3499.0, 15.0)];
        let asks = vec![(3501.0, 8.0), (3502.0, 12.0)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        // 測試增量更新
        let bid_updates = vec![(3500.5, 5.0)]; // 新的最佳買價
        let ask_updates = vec![(3500.8, 3.0)]; // 新的最佳賣價

        book.apply_update(&bid_updates, &ask_updates, 2000).unwrap();

        assert_eq!(book.best_bid(), Some(3500.5));
        assert_eq!(book.best_ask(), Some(3500.8));
        let spread = book.spread().unwrap();
        assert!(
            (spread - 0.3).abs() < 1e-10,
            "Spread should be approximately 0.3, got {}",
            spread
        );
    }

    #[test]
    fn test_dual_joiner_arbitrage() {
        let mut joiner = DualTopNJoiner::<5>::new(
            Symbol::new("BTCUSDT_BINANCE"),
            Symbol::new("BTCUSDT_BITGET"),
            0.2, // 0.2 bps 最小套利價差（更容易觸發）
        );

        // Binance: bid=67200, ask=67201
        let bids_a = vec![(67200.0, 1.0)];
        let asks_a = vec![(67201.0, 1.0)];
        joiner
            .venue_a
            .apply_snapshot(&bids_a, &asks_a, 1000)
            .unwrap();

        // Bitget: bid=67203, ask=67204 （比 Binance 高）
        let bids_b = vec![(67203.0, 1.0)];
        let asks_b = vec![(67204.0, 1.0)];
        joiner
            .venue_b
            .apply_snapshot(&bids_b, &asks_b, 1000)
            .unwrap();

        // 檢查套利機會：Binance 買入，Bitget 賣出
        let arb = joiner.check_arbitrage(1500);

        assert!(arb.is_some(), "Should find arbitrage opportunity");
        let (venue_a_buy, profit_bps) = arb.unwrap();
        assert!(venue_a_buy); // Venue A (Binance) 買入
        assert!(profit_bps > 0.2); // 利潤超過 0.2 bps
    }

    // ============================================================================
    // Additional edge case tests
    // ============================================================================

    #[test]
    fn test_empty_orderbook() {
        let book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());
        assert!(book.spread().is_none());
        assert!(book.mid_price().is_none());
        assert!(!book.is_valid());
        assert_eq!(book.bid_count, 0);
        assert_eq!(book.ask_count, 0);
    }

    #[test]
    fn test_snapshot_overwrites_previous() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("ETHUSDT"));

        // First snapshot
        let bids1 = vec![(3500.0, 10.0)];
        let asks1 = vec![(3501.0, 8.0)];
        book.apply_snapshot(&bids1, &asks1, 1000).unwrap();

        assert_eq!(book.best_bid(), Some(3500.0));
        assert_eq!(book.version, 1);

        // Second snapshot should completely overwrite
        let bids2 = vec![(4000.0, 5.0), (3999.0, 3.0)];
        let asks2 = vec![(4001.0, 4.0)];
        book.apply_snapshot(&bids2, &asks2, 2000).unwrap();

        assert_eq!(book.best_bid(), Some(4000.0));
        assert_eq!(book.bid_count, 2);
        assert_eq!(book.ask_count, 1);
        assert_eq!(book.version, 2);
    }

    #[test]
    fn test_update_removes_level_on_zero_quantity() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(67200.0, 1.5), (67199.0, 2.0)];
        let asks = vec![(67201.0, 1.2)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        assert_eq!(book.bid_count, 2);

        // Remove first bid level by setting quantity to 0
        let bid_updates = vec![(67200.0, 0.0)];
        book.apply_update(&bid_updates, &[], 2000).unwrap();

        assert_eq!(book.bid_count, 1);
        assert_eq!(book.best_bid(), Some(67199.0));
    }

    #[test]
    fn test_update_inserts_new_level_in_order() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(67200.0, 1.0), (67198.0, 1.0)];
        let asks = vec![(67201.0, 1.0)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        // Insert new bid in the middle
        let bid_updates = vec![(67199.0, 2.0)];
        book.apply_update(&bid_updates, &[], 2000).unwrap();

        assert_eq!(book.bid_count, 3);
        // Should be ordered: 67200, 67199, 67198
        assert_eq!(book.bid_prices[0], 67200.0);
        assert_eq!(book.bid_prices[1], 67199.0);
        assert_eq!(book.bid_prices[2], 67198.0);
    }

    #[test]
    fn test_topn_capacity_limit() {
        let mut book = TopNOrderBook::<3>::new(Symbol::new("BTCUSDT"));

        // Try to insert more than 3 levels
        let bids = vec![
            (67200.0, 1.0),
            (67199.0, 1.0),
            (67198.0, 1.0),
            (67197.0, 1.0), // Should be ignored
            (67196.0, 1.0), // Should be ignored
        ];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        assert_eq!(book.bid_count, 3);
        assert_eq!(book.bid_prices[2], 67198.0); // Lowest kept bid
    }

    #[test]
    fn test_vwap_calculation() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![
            (100.0, 10.0), // 10 units at 100
            (99.0, 20.0),  // 20 units at 99
            (98.0, 30.0),  // 30 units at 98
        ];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        // VWAP for 15 units: (10*100 + 5*99) / 15 = 1495/15 = 99.666...
        let vwap = book.vwap_bid(15.0).unwrap();
        assert!((vwap - 99.666666).abs() < 0.001);

        // VWAP for 5 units: all at best bid
        let vwap_small = book.vwap_bid(5.0).unwrap();
        assert_eq!(vwap_small, 100.0);
    }

    #[test]
    fn test_vwap_insufficient_liquidity() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(100.0, 5.0)];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        // Request more than available: should return VWAP of available
        let vwap = book.vwap_bid(100.0).unwrap();
        assert_eq!(vwap, 100.0);
    }

    #[test]
    fn test_dual_joiner_no_arbitrage() {
        let mut joiner = DualTopNJoiner::<5>::new(
            Symbol::new("BTCUSDT_A"),
            Symbol::new("BTCUSDT_B"),
            1.0, // 1 bps minimum
        );

        // Same prices - no arbitrage
        joiner
            .venue_a
            .apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 1000)
            .unwrap();
        joiner
            .venue_b
            .apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 1000)
            .unwrap();

        assert!(joiner.check_arbitrage(1500).is_none());
    }

    #[test]
    fn test_dual_joiner_stale_data_rejection() {
        let mut joiner = DualTopNJoiner::<5>::new(
            Symbol::new("BTCUSDT_A"),
            Symbol::new("BTCUSDT_B"),
            0.1,
        );
        joiner.max_stale_us = 1000; // 1ms staleness limit

        // Venue A is fresh, Venue B is stale
        joiner
            .venue_a
            .apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 2000)
            .unwrap();
        joiner
            .venue_b
            .apply_snapshot(&[(99.0, 1.0)], &[(100.5, 1.0)], 500)
            .unwrap(); // Old timestamp

        // Current time is 2500, Venue B data is 2000us old (stale)
        assert!(joiner.check_arbitrage(2500).is_none());
    }

    #[test]
    fn test_bid_level_access() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(100.0, 1.0), (99.0, 2.0), (98.0, 3.0)];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        assert_eq!(book.bid_level(0), Some((100.0, 1.0)));
        assert_eq!(book.bid_level(1), Some((99.0, 2.0)));
        assert_eq!(book.bid_level(2), Some((98.0, 3.0)));
        assert_eq!(book.bid_level(3), None);
    }

    #[test]
    fn test_clear_orderbook() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(100.0, 1.0)];
        let asks = vec![(101.0, 1.0)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        assert!(book.is_valid());

        book.clear();

        assert!(!book.is_valid());
        assert_eq!(book.bid_count, 0);
        assert_eq!(book.ask_count, 0);
    }
}
