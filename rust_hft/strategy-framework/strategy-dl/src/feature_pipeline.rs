//! 特徵流水線：TopN → 歸一化處理
//!
//! 根據 PRD 要求實現：
//! - TopN 訂單簿特徵提取
//! - 相對 mid、量比、失衡、斜率計算
//! - 可選時間窗口聚合
//! - Z-score/MinMax/Robust 歸一化

use crate::config::FeatureConfig;
use hft_core::{HftError, HftResult, Timestamp};
use ndarray::Array1;
use ports::MarketEvent;
use std::collections::VecDeque;
use tracing::{debug, warn};

/// 特徵提取器
pub struct FeatureExtractor {
    config: FeatureConfig,
    // 滑動窗口數據
    price_window: VecDeque<f64>,
    volume_window: VecDeque<f64>,
    imbalance_window: VecDeque<f64>,
    timestamp_window: VecDeque<Timestamp>,

    // 歸一化統計量 (滑動更新)
    price_stats: RunningStats,
    volume_stats: RunningStats,
    imbalance_stats: RunningStats,

    // 特徵緩存
    last_features: Option<Array1<f32>>,
    feature_count: usize,
}

/// 特徵流水線 (多品種聚合)
pub struct FeaturePipeline {
    extractors: std::collections::HashMap<String, FeatureExtractor>, // symbol -> extractor
    config: FeatureConfig,
}

/// 滑動統計量
#[derive(Debug, Clone)]
struct RunningStats {
    mean: f64,
    variance: f64,
    min: f64,
    max: f64,
    count: usize,
    window_size: usize,
}

/// TopN 訂單簿快照
#[derive(Debug, Clone)]
pub struct TopNSnapshot {
    pub bids: Vec<(f64, f64)>, // (price, quantity)
    pub asks: Vec<(f64, f64)>,
    pub timestamp: Timestamp,
    pub mid_price: f64,
    pub spread: f64,
    pub total_bid_volume: f64,
    pub total_ask_volume: f64,
}

impl FeatureExtractor {
    /// 創建新的特徵提取器
    pub fn new(config: FeatureConfig) -> Self {
        let window_size = config.window_size.unwrap_or(30);

        Self {
            config,
            price_window: VecDeque::with_capacity(window_size),
            volume_window: VecDeque::with_capacity(window_size),
            imbalance_window: VecDeque::with_capacity(window_size),
            timestamp_window: VecDeque::with_capacity(window_size),

            price_stats: RunningStats::new(window_size),
            volume_stats: RunningStats::new(window_size),
            imbalance_stats: RunningStats::new(window_size),

            last_features: None,
            feature_count: 0,
        }
    }

    /// 從市場事件提取特徵
    pub fn extract_features(&mut self, event: &MarketEvent) -> HftResult<Option<Array1<f32>>> {
        match event {
            MarketEvent::Snapshot(snapshot) => {
                let topn = self.convert_to_topn(snapshot)?;
                self.process_topn_features(&topn)
            }
            MarketEvent::Update(_update) => {
                // 對於增量更新，可以考慮重構 TopN
                debug!("收到增量更新，暫時跳過特徵提取");
                Ok(None)
            }
            _ => {
                // 其他事件類型暫不處理
                Ok(None)
            }
        }
    }

    /// 轉換市場快照為 TopN 格式
    fn convert_to_topn(&self, snapshot: &ports::MarketSnapshot) -> HftResult<TopNSnapshot> {
        let n = self
            .config
            .top_n
            .min(snapshot.bids.len())
            .min(snapshot.asks.len());

        if n == 0 {
            return Err(HftError::Parse("訂單簿為空".to_string()));
        }

        let mut bids: Vec<(f64, f64)> = snapshot
            .bids
            .iter()
            .take(n)
            .map(|level| {
                (
                    level.price.0.to_string().parse::<f64>().unwrap_or(0.0),
                    level.quantity.0.to_string().parse::<f64>().unwrap_or(0.0),
                )
            })
            .collect();

        let mut asks: Vec<(f64, f64)> = snapshot
            .asks
            .iter()
            .take(n)
            .map(|level| {
                (
                    level.price.0.to_string().parse::<f64>().unwrap_or(0.0),
                    level.quantity.0.to_string().parse::<f64>().unwrap_or(0.0),
                )
            })
            .collect();

        // 確保價格排序正確
        bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)); // 降序
        asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)); // 升序

        let best_bid = bids.first().map(|x| x.0).unwrap_or(0.0);
        let best_ask = asks.first().map(|x| x.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        let spread = best_ask - best_bid;

        let total_bid_volume: f64 = bids.iter().map(|x| x.1).sum();
        let total_ask_volume: f64 = asks.iter().map(|x| x.1).sum();

        Ok(TopNSnapshot {
            bids,
            asks,
            timestamp: snapshot.timestamp,
            mid_price,
            spread,
            total_bid_volume,
            total_ask_volume,
        })
    }

    /// 處理 TopN 特徵提取
    fn process_topn_features(&mut self, topn: &TopNSnapshot) -> HftResult<Option<Array1<f32>>> {
        // 計算基礎特徵
        let imbalance = self.calculate_imbalance(topn);

        // 更新滑動窗口
        self.update_windows(topn, imbalance);

        // 需要足夠的歷史數據才能計算特徵
        if self.price_window.len() < 10 {
            return Ok(None);
        }

        // 提取各類特徵
        let mut features = Vec::new();

        if self.config.include_price {
            features.extend(self.extract_price_features(topn)?);
        }

        if self.config.include_volume {
            features.extend(self.extract_volume_features(topn)?);
        }

        if self.config.include_imbalance {
            features.extend(self.extract_imbalance_features(topn)?);
        }

        if self.config.include_slope {
            features.extend(self.extract_slope_features()?);
        }

        // 應用歸一化
        let normalized_features = self.normalize_features(features)?;

        // 更新緩存
        self.last_features = Some(normalized_features.clone());
        self.feature_count += 1;

        debug!(
            "提取特徵完成: {} 維，總計 {} 次",
            normalized_features.len(),
            self.feature_count
        );

        Ok(Some(normalized_features))
    }

    /// 計算訂單簿失衡度
    fn calculate_imbalance(&self, topn: &TopNSnapshot) -> f64 {
        if topn.total_bid_volume + topn.total_ask_volume == 0.0 {
            return 0.0;
        }

        (topn.total_bid_volume - topn.total_ask_volume)
            / (topn.total_bid_volume + topn.total_ask_volume)
    }

    /// 更新滑動窗口
    fn update_windows(&mut self, topn: &TopNSnapshot, imbalance: f64) {
        let window_size = self.config.window_size.unwrap_or(30);

        // 添加新數據
        self.price_window.push_back(topn.mid_price);
        self.volume_window
            .push_back(topn.total_bid_volume + topn.total_ask_volume);
        self.imbalance_window.push_back(imbalance);
        self.timestamp_window.push_back(topn.timestamp);

        // 維護窗口大小
        while self.price_window.len() > window_size {
            self.price_window.pop_front();
            self.volume_window.pop_front();
            self.imbalance_window.pop_front();
            self.timestamp_window.pop_front();
        }

        // 更新統計量
        self.price_stats.update(topn.mid_price);
        self.volume_stats
            .update(topn.total_bid_volume + topn.total_ask_volume);
        self.imbalance_stats.update(imbalance);
    }

    /// 提取價格特徵
    fn extract_price_features(&self, topn: &TopNSnapshot) -> HftResult<Vec<f32>> {
        let mut features = Vec::new();

        // 相對 mid 價格特徵 (前 N 檔)
        for i in 0..self.config.top_n.min(topn.bids.len()) {
            let bid_relative = (topn.bids[i].0 - topn.mid_price) / topn.mid_price;
            let ask_relative = (topn.asks[i].0 - topn.mid_price) / topn.mid_price;

            features.push(bid_relative as f32);
            features.push(ask_relative as f32);
        }

        // 價差特徵
        features.push((topn.spread / topn.mid_price) as f32); // 相對價差

        // 價格變化率 (如果有歷史數據)
        if let Some(&last_price) = self
            .price_window
            .get(self.price_window.len().saturating_sub(2))
        {
            let price_change = (topn.mid_price - last_price) / last_price;
            features.push(price_change as f32);
        } else {
            features.push(0.0);
        }

        Ok(features)
    }

    /// 提取量比特徵
    fn extract_volume_features(&self, topn: &TopNSnapshot) -> HftResult<Vec<f32>> {
        let mut features = Vec::new();

        // 各檔量比特徵
        for i in 0..self.config.top_n.min(topn.bids.len()) {
            let bid_qty_ratio = if topn.total_bid_volume > 0.0 {
                topn.bids[i].1 / topn.total_bid_volume
            } else {
                0.0
            };

            let ask_qty_ratio = if topn.total_ask_volume > 0.0 {
                topn.asks[i].1 / topn.total_ask_volume
            } else {
                0.0
            };

            features.push(bid_qty_ratio as f32);
            features.push(ask_qty_ratio as f32);
        }

        // 總量特徵
        let total_volume = topn.total_bid_volume + topn.total_ask_volume;
        features.push(total_volume as f32);

        // 量變化率
        if let Some(&last_volume) = self
            .volume_window
            .get(self.volume_window.len().saturating_sub(2))
        {
            let volume_change = if last_volume > 0.0 {
                (total_volume - last_volume) / last_volume
            } else {
                0.0
            };
            features.push(volume_change as f32);
        } else {
            features.push(0.0);
        }

        Ok(features)
    }

    /// 提取失衡特徵
    fn extract_imbalance_features(&self, topn: &TopNSnapshot) -> HftResult<Vec<f32>> {
        let mut features = Vec::new();

        // 當前失衡度
        let current_imbalance = self.calculate_imbalance(topn);
        features.push(current_imbalance as f32);

        // 買賣比
        let bid_ask_ratio = if topn.total_ask_volume > 0.0 {
            topn.total_bid_volume / topn.total_ask_volume
        } else {
            f64::INFINITY
        };

        features.push(bid_ask_ratio.ln() as f32); // 取對數避免極值

        // 失衡變化率
        if self.imbalance_window.len() >= 2 {
            let last_imbalance = self.imbalance_window[self.imbalance_window.len() - 2];
            let imbalance_change = current_imbalance - last_imbalance;
            features.push(imbalance_change as f32);
        } else {
            features.push(0.0);
        }

        Ok(features)
    }

    /// 提取斜率特徵 (時間序列趨勢)
    fn extract_slope_features(&self) -> HftResult<Vec<f32>> {
        let mut features = Vec::new();

        // 價格斜率 (線性回歸)
        if self.price_window.len() >= 5 {
            let price_slope = self.calculate_slope(&self.price_window);
            features.push(price_slope as f32);
        } else {
            features.push(0.0);
        }

        // 量斜率
        if self.volume_window.len() >= 5 {
            let volume_slope = self.calculate_slope(&self.volume_window);
            features.push(volume_slope as f32);
        } else {
            features.push(0.0);
        }

        // 失衡斜率
        if self.imbalance_window.len() >= 5 {
            let imbalance_slope = self.calculate_slope(&self.imbalance_window);
            features.push(imbalance_slope as f32);
        } else {
            features.push(0.0);
        }

        Ok(features)
    }

    /// 計算線性斜率 (最小二乘法)
    fn calculate_slope(&self, data: &VecDeque<f64>) -> f64 {
        let n = data.len();
        if n < 2 {
            return 0.0;
        }

        let x_sum: f64 = (0..n).map(|i| i as f64).sum();
        let y_sum: f64 = data.iter().sum();
        let xy_sum: f64 = data.iter().enumerate().map(|(i, &y)| i as f64 * y).sum();
        let x2_sum: f64 = (0..n).map(|i| (i as f64) * (i as f64)).sum();

        let n_f = n as f64;
        let denominator = n_f * x2_sum - x_sum * x_sum;

        if denominator.abs() < f64::EPSILON {
            return 0.0;
        }

        (n_f * xy_sum - x_sum * y_sum) / denominator
    }

    /// 歸一化特徵
    fn normalize_features(&self, features: Vec<f32>) -> HftResult<Array1<f32>> {
        let mut normalized = Array1::from(features);

        match self.config.normalization.as_str() {
            "zscore" => {
                // Z-score 歸一化：(x - μ) / σ
                let mean = normalized.mean().unwrap_or(0.0);
                let std = normalized.std(0.0);

                if std > f32::EPSILON {
                    normalized = (normalized - mean) / std;
                }
            }
            "minmax" => {
                // Min-Max 歸一化：(x - min) / (max - min)
                let mut min_val = f32::INFINITY;
                let mut max_val = f32::NEG_INFINITY;
                for &value in normalized.iter() {
                    if value.is_finite() {
                        if value < min_val {
                            min_val = value;
                        }
                        if value > max_val {
                            max_val = value;
                        }
                    }
                }
                if min_val.is_finite() && max_val.is_finite() {
                    let range = max_val - min_val;
                    if range > f32::EPSILON {
                        normalized = (normalized - min_val) / range;
                    }
                }
            }
            "robust" => {
                // Robust 歸一化：基於中位數和 IQR
                let mut sorted_features = normalized.to_vec();
                sorted_features
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

                let q1_idx = sorted_features.len() / 4;
                let q3_idx = sorted_features.len() * 3 / 4;
                let median_idx = sorted_features.len() / 2;

                if sorted_features.len() > 4 {
                    let median = sorted_features[median_idx];
                    let iqr = sorted_features[q3_idx] - sorted_features[q1_idx];

                    if iqr > f32::EPSILON {
                        normalized = (normalized - median) / iqr;
                    }
                }
            }
            _ => {
                warn!(
                    "未知的歸一化方法: {}，跳過歸一化",
                    self.config.normalization
                );
            }
        }

        // 裁剪極值以避免梯度爆炸
        normalized = normalized.mapv(|x| x.max(-10.0).min(10.0));

        Ok(normalized)
    }

    /// 獲取最後提取的特徵
    pub fn get_last_features(&self) -> Option<&Array1<f32>> {
        self.last_features.as_ref()
    }

    /// 獲取特徵提取統計
    pub fn get_stats(&self) -> FeatureExtractorStats {
        FeatureExtractorStats {
            feature_count: self.feature_count,
            window_size: self.price_window.len(),
            last_price: self.price_window.back().copied(),
            last_imbalance: self.imbalance_window.back().copied(),
        }
    }
}

/// 特徵提取統計
#[derive(Debug, Clone)]
pub struct FeatureExtractorStats {
    pub feature_count: usize,
    pub window_size: usize,
    pub last_price: Option<f64>,
    pub last_imbalance: Option<f64>,
}

impl FeaturePipeline {
    /// 創建新的特徵流水線
    pub fn new(config: FeatureConfig) -> Self {
        Self {
            extractors: std::collections::HashMap::new(),
            config,
        }
    }

    /// 為品種添加提取器
    pub fn add_symbol(&mut self, symbol: String) {
        if !self.extractors.contains_key(&symbol) {
            let extractor = FeatureExtractor::new(self.config.clone());
            self.extractors.insert(symbol, extractor);
        }
    }

    /// 處理市場事件並提取特徵
    pub fn process_event(
        &mut self,
        symbol: &str,
        event: &MarketEvent,
    ) -> HftResult<Option<Array1<f32>>> {
        if let Some(extractor) = self.extractors.get_mut(symbol) {
            extractor.extract_features(event)
        } else {
            Err(HftError::Config(format!(
                "未找到品種的特徵提取器: {}",
                symbol
            )))
        }
    }

    /// 獲取所有品種的統計信息
    pub fn get_all_stats(&self) -> std::collections::HashMap<String, FeatureExtractorStats> {
        self.extractors
            .iter()
            .map(|(symbol, extractor)| (symbol.clone(), extractor.get_stats()))
            .collect()
    }
}

impl RunningStats {
    fn new(window_size: usize) -> Self {
        Self {
            mean: 0.0,
            variance: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            count: 0,
            window_size,
        }
    }

    fn update(&mut self, value: f64) {
        self.count = (self.count + 1).min(self.window_size);

        // 簡化的滑動平均更新
        let alpha = 1.0 / (self.count as f64);
        let delta = value - self.mean;
        self.mean += alpha * delta;
        self.variance += alpha * (delta * delta - self.variance);

        // 更新極值
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    #[allow(dead_code)]
    fn std(&self) -> f64 {
        self.variance.sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::Symbol;
    use ports::{BookLevel, MarketSnapshot};

    fn create_test_snapshot() -> MarketSnapshot {
        MarketSnapshot {
            symbol: Symbol("BTCUSDT".to_string()),
            timestamp: 1_000_000,
            bids: vec![
                BookLevel::new_unchecked(100.0, 10.0),
                BookLevel::new_unchecked(99.0, 5.0),
            ],
            asks: vec![
                BookLevel::new_unchecked(101.0, 8.0),
                BookLevel::new_unchecked(102.0, 12.0),
            ],
            sequence: 1,
            source_venue: None,
        }
    }

    #[test]
    fn test_feature_extractor_creation() {
        let config = FeatureConfig::default();
        let extractor = FeatureExtractor::new(config);

        assert_eq!(extractor.feature_count, 0);
        assert!(extractor.last_features.is_none());
    }

    #[test]
    fn test_topn_conversion() {
        let config = FeatureConfig::default();
        let extractor = FeatureExtractor::new(config);
        let snapshot = create_test_snapshot();

        let topn = extractor.convert_to_topn(&snapshot).unwrap();

        assert_eq!(topn.bids.len(), 2);
        assert_eq!(topn.asks.len(), 2);
        assert_eq!(topn.mid_price, 100.5); // (100 + 101) / 2
        assert_eq!(topn.spread, 1.0); // 101 - 100
    }

    #[test]
    fn test_imbalance_calculation() {
        let config = FeatureConfig::default();
        let extractor = FeatureExtractor::new(config);
        let snapshot = create_test_snapshot();
        let topn = extractor.convert_to_topn(&snapshot).unwrap();

        let imbalance = extractor.calculate_imbalance(&topn);

        // total_bid_volume = 10 + 5 = 15
        // total_ask_volume = 8 + 12 = 20
        // imbalance = (15 - 20) / (15 + 20) = -5/35 ≈ -0.143
        assert!((imbalance + 0.142857).abs() < 0.001);
    }

    #[test]
    fn test_slope_calculation() {
        let config = FeatureConfig::default();
        let extractor = FeatureExtractor::new(config);

        let data: VecDeque<f64> = [1.0, 2.0, 3.0, 4.0, 5.0].iter().copied().collect();
        let slope = extractor.calculate_slope(&data);

        // 完美線性數據的斜率應該是 1.0
        assert!((slope - 1.0).abs() < 0.001);
    }
}
