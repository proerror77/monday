/*!
 * Traffic-Based Grouping Algorithm
 * 
 * 基於實時流量數據的智能分組算法
 * 實現類似 barter-rs 的批次化管理，但專門針對單交易所多商品場景優化
 */

use crate::integrations::{BitgetChannel, SymbolTrafficStats};
use std::collections::{HashMap, BTreeMap};
use tracing::{info, debug, warn};

/// 流量分組結果
#[derive(Debug, Clone)]
pub struct TrafficGrouping {
    pub groups: Vec<TrafficGroup>,
    pub total_load: f64,
    pub load_balance_ratio: f64,
    pub grouping_strategy: String,
}

/// 單個流量組
#[derive(Debug, Clone)]
pub struct TrafficGroup {
    pub group_id: usize,
    pub symbols: Vec<String>,
    pub total_load: f64,
    pub group_type: GroupType,
    pub priority: GroupPriority,
}

/// 組類型
#[derive(Debug, Clone, PartialEq)]
pub enum GroupType {
    HighTraffic,    // 高流量組 (>15 msg/s)
    MediumTraffic,  // 中等流量組 (8-15 msg/s)
    LowTraffic,     // 低流量組 (<8 msg/s)
    Mixed,          // 混合組
}

/// 組優先級
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum GroupPriority {
    Critical = 1,   // 關鍵組（高流量獨立）
    High = 2,       // 高優先級
    Medium = 3,     // 中等優先級  
    Low = 4,        // 低優先級
}

/// 流量分組算法配置
#[derive(Debug, Clone)]
pub struct GroupingConfig {
    pub high_traffic_threshold: f64,
    pub medium_traffic_threshold: f64,
    pub max_load_per_group: f64,
    pub target_groups: usize,
    pub balance_tolerance: f64,
    pub allow_mixed_groups: bool,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        Self {
            high_traffic_threshold: 15.0,
            medium_traffic_threshold: 8.0,
            max_load_per_group: 25.0,
            target_groups: 4,
            balance_tolerance: 0.8,
            allow_mixed_groups: true,
        }
    }
}

/// 智能流量分組器
pub struct TrafficBasedGrouper {
    config: GroupingConfig,
}

impl TrafficBasedGrouper {
    pub fn new(config: GroupingConfig) -> Self {
        Self { config }
    }
    
    /// 基於實時流量數據進行分組
    pub fn group_by_traffic(&self, traffic_stats: &HashMap<String, SymbolTrafficStats>) -> TrafficGrouping {
        info!("🧠 開始智能流量分組算法...");
        
        // 1. 預處理：按流量排序並分類
        let categorized_symbols = self.categorize_symbols(traffic_stats);
        
        // 2. 應用分組策略
        let groups = self.apply_grouping_strategy(&categorized_symbols);
        
        // 3. 優化負載均衡
        let optimized_groups = self.optimize_load_balance(groups);
        
        // 4. 計算統計信息
        let total_load: f64 = optimized_groups.iter().map(|g| g.total_load).sum();
        let load_balance_ratio = self.calculate_load_balance_ratio(&optimized_groups);
        
        let grouping = TrafficGrouping {
            groups: optimized_groups,
            total_load,
            load_balance_ratio,
            grouping_strategy: "TrafficBased".to_string(),
        };
        
        self.log_grouping_result(&grouping);
        grouping
    }
    
    /// 符號分類：按流量大小分為高、中、低三類
    fn categorize_symbols(&self, traffic_stats: &HashMap<String, SymbolTrafficStats>) -> CategorizedSymbols {
        let mut high_traffic = Vec::new();
        let mut medium_traffic = Vec::new();
        let mut low_traffic = Vec::new();
        
        // 按流量排序
        let mut sorted_symbols: Vec<_> = traffic_stats.iter().collect();
        sorted_symbols.sort_by(|a, b| b.1.messages_per_second.partial_cmp(&a.1.messages_per_second).unwrap());
        
        for (symbol, stats) in sorted_symbols {
            let rate = stats.messages_per_second;
            
            if rate >= self.config.high_traffic_threshold {
                high_traffic.push((symbol.clone(), rate));
            } else if rate >= self.config.medium_traffic_threshold {
                medium_traffic.push((symbol.clone(), rate));
            } else {
                low_traffic.push((symbol.clone(), rate));
            }
        }
        
        debug!("分類完成 - 高流量: {}, 中流量: {}, 低流量: {}", 
               high_traffic.len(), medium_traffic.len(), low_traffic.len());
        
        CategorizedSymbols { high_traffic, medium_traffic, low_traffic }
    }
    
    /// 應用分組策略
    fn apply_grouping_strategy(&self, categorized: &CategorizedSymbols) -> Vec<TrafficGroup> {
        let mut groups = Vec::new();
        let mut group_id = 0;
        
        // 策略1: 高流量符號獨立成組或小組合併
        for (symbol, rate) in &categorized.high_traffic {
            if rate >= &20.0 {
                // 超高流量：獨立組
                groups.push(TrafficGroup {
                    group_id,
                    symbols: vec![symbol.clone()],
                    total_load: *rate,
                    group_type: GroupType::HighTraffic,
                    priority: GroupPriority::Critical,
                });
                group_id += 1;
            } else {
                // 高流量：嘗試與其他高流量合併
                let group_idx = self.find_suitable_group(&mut groups, *rate, GroupType::HighTraffic);
                if group_idx != usize::MAX {
                    groups[group_idx].symbols.push(symbol.clone());
                    groups[group_idx].total_load += rate;
                    // 如果不是同類型，設置為混合組
                    if groups[group_idx].group_type != GroupType::HighTraffic {
                        groups[group_idx].group_type = GroupType::Mixed;
                    }
                } else {
                    groups.push(TrafficGroup {
                        group_id,
                        symbols: vec![symbol.clone()],
                        total_load: *rate,
                        group_type: GroupType::HighTraffic,
                        priority: GroupPriority::High,
                    });
                    group_id += 1;
                }
            }
        }
        
        // 策略2: 中流量符號優化合併
        for (symbol, rate) in &categorized.medium_traffic {
            let group_idx = self.find_suitable_group(&mut groups, *rate, GroupType::MediumTraffic);
            if group_idx != usize::MAX {
                groups[group_idx].symbols.push(symbol.clone());
                groups[group_idx].total_load += rate;
                // 如果不是同類型，設置為混合組
                if groups[group_idx].group_type != GroupType::MediumTraffic {
                    groups[group_idx].group_type = GroupType::Mixed;
                }
            } else {
                groups.push(TrafficGroup {
                    group_id,
                    symbols: vec![symbol.clone()],
                    total_load: *rate,
                    group_type: GroupType::MediumTraffic,
                    priority: GroupPriority::Medium,
                });
                group_id += 1;
            }
        }
        
        // 策略3: 低流量符號大量合併
        let mut low_traffic_group: Option<TrafficGroup> = None;
        for (symbol, rate) in &categorized.low_traffic {
            if let Some(ref mut group) = low_traffic_group {
                if group.total_load + rate <= self.config.max_load_per_group {
                    group.symbols.push(symbol.clone());
                    group.total_load += rate;
                    continue;
                }
            }
            
            // 創建新的低流量組
            let new_group = TrafficGroup {
                group_id,
                symbols: vec![symbol.clone()],
                total_load: *rate,
                group_type: GroupType::LowTraffic,
                priority: GroupPriority::Low,
            };
            
            if low_traffic_group.is_none() {
                low_traffic_group = Some(new_group);
            } else {
                groups.push(low_traffic_group.take().unwrap());
                low_traffic_group = Some(new_group);
                group_id += 1;
            }
        }
        
        if let Some(group) = low_traffic_group {
            groups.push(group);
        }
        
        // 重新分配組ID
        for (i, group) in groups.iter_mut().enumerate() {
            group.group_id = i;
        }
        
        groups
    }
    
    /// 尋找合適的現有組進行合併
    fn find_suitable_group(&self, groups: &mut Vec<TrafficGroup>, rate: f64, prefer_type: GroupType) -> usize {
        // 首先嘗試找同類型的組
        for (i, group) in groups.iter().enumerate() {
            if group.group_type == prefer_type && group.total_load + rate <= self.config.max_load_per_group {
                return i;
            }
        }
        
        // 如果允許混合組，尋找任何合適的組
        if self.config.allow_mixed_groups {
            for (i, group) in groups.iter().enumerate() {
                if group.total_load + rate <= self.config.max_load_per_group {
                    return i;
                }
            }
        }
        
        usize::MAX // 表示沒有找到合適的組
    }
    
    /// 優化負載均衡
    fn optimize_load_balance(&self, groups: Vec<TrafficGroup>) -> Vec<TrafficGroup> {
        debug!("🔧 開始負載均衡優化...");
        
        let current_balance = self.calculate_load_balance_ratio(&groups);
        if current_balance >= self.config.balance_tolerance {
            debug!("當前負載均衡已滿足要求: {:.2}", current_balance);
            return groups;
        }
        
        // 使用貪心算法進行負載重新分配
        let mut optimized_groups = self.greedy_load_balancing(&groups);
        
        // 如果仍不滿足，嘗試拆分大組
        let new_balance = self.calculate_load_balance_ratio(&optimized_groups);
        if new_balance < self.config.balance_tolerance {
            optimized_groups = self.split_heavy_groups(optimized_groups);
        }
        
        optimized_groups
    }
    
    /// 貪心負載均衡算法
    fn greedy_load_balancing(&self, groups: &[TrafficGroup]) -> Vec<TrafficGroup> {
        // 收集所有符號及其流量
        let mut symbol_loads = Vec::new();
        for group in groups {
            for symbol in &group.symbols {
                // 計算單個符號的大致流量 (總負載 / 符號數)
                let approx_load = group.total_load / group.symbols.len() as f64;
                symbol_loads.push((symbol.clone(), approx_load));
            }
        }
        
        // 按負載降序排序
        symbol_loads.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        // 重新分配到最少負載的組
        let mut new_groups: Vec<TrafficGroup> = Vec::new();
        for _ in 0..std::cmp::min(self.config.target_groups, groups.len()) {
            new_groups.push(TrafficGroup {
                group_id: new_groups.len(),
                symbols: Vec::new(),
                total_load: 0.0,
                group_type: GroupType::Mixed,
                priority: GroupPriority::Medium,
            });
        }
        
        for (symbol, load) in symbol_loads {
            // 找到負載最小的組
            let min_group = new_groups
                .iter_mut()
                .min_by(|a, b| a.total_load.partial_cmp(&b.total_load).unwrap())
                .unwrap();
            
            min_group.symbols.push(symbol);
            min_group.total_load += load;
        }
        
        new_groups
    }
    
    /// 拆分負載過重的組
    fn split_heavy_groups(&self, groups: Vec<TrafficGroup>) -> Vec<TrafficGroup> {
        let mut new_groups = Vec::new();
        
        for group in groups {
            if group.total_load > self.config.max_load_per_group && group.symbols.len() > 1 {
                // 拆分為兩個組
                let mid = group.symbols.len() / 2;
                let (left_symbols, right_symbols) = group.symbols.split_at(mid);
                
                let left_load = group.total_load * (left_symbols.len() as f64 / group.symbols.len() as f64);
                let right_load = group.total_load - left_load;
                
                new_groups.push(TrafficGroup {
                    group_id: new_groups.len(),
                    symbols: left_symbols.to_vec(),
                    total_load: left_load,
                    group_type: group.group_type.clone(),
                    priority: group.priority.clone(),
                });
                
                new_groups.push(TrafficGroup {
                    group_id: new_groups.len(),
                    symbols: right_symbols.to_vec(),
                    total_load: right_load,
                    group_type: group.group_type,
                    priority: group.priority,
                });
            } else {
                new_groups.push(TrafficGroup {
                    group_id: new_groups.len(),
                    ..group
                });
            }
        }
        
        new_groups
    }
    
    /// 計算負載均衡比
    fn calculate_load_balance_ratio(&self, groups: &[TrafficGroup]) -> f64 {
        if groups.is_empty() {
            return 1.0;
        }
        
        let loads: Vec<f64> = groups.iter().map(|g| g.total_load).collect();
        let max_load = loads.iter().fold(0.0_f64, |a, &b| a.max(b));
        let min_load = loads.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        
        if max_load > 0.0 {
            min_load / max_load
        } else {
            1.0
        }
    }
    
    /// 記錄分組結果
    fn log_grouping_result(&self, grouping: &TrafficGrouping) {
        info!("📊 === 智能流量分組結果 ===");
        info!("總負載: {:.1} msg/s", grouping.total_load);
        info!("負載均衡比: {:.2}", grouping.load_balance_ratio);
        info!("分組數量: {}", grouping.groups.len());
        
        for group in &grouping.groups {
            info!("  🔗 組 {}: {:?} - {:.1} msg/s - {} 符號", 
                  group.group_id, 
                  group.group_type,
                  group.total_load,
                  group.symbols.len());
            info!("     符號: {:?}", group.symbols);
        }
        info!("========================");
    }
}

/// 分類後的符號
#[derive(Debug)]
struct CategorizedSymbols {
    high_traffic: Vec<(String, f64)>,
    medium_traffic: Vec<(String, f64)>,
    low_traffic: Vec<(String, f64)>,
}

/// 創建針對 HFT 優化的分組器
pub fn create_hft_optimized_grouper() -> TrafficBasedGrouper {
    let config = GroupingConfig {
        high_traffic_threshold: 15.0,
        medium_traffic_threshold: 8.0,
        max_load_per_group: 30.0,    // HFT 需要更高的負載容量
        target_groups: 4,
        balance_tolerance: 0.85,      // 更嚴格的均衡要求
        allow_mixed_groups: true,
    };
    
    TrafficBasedGrouper::new(config)
}

/// 創建保守的分組器（適用於穩定性優先場景）
pub fn create_conservative_grouper() -> TrafficBasedGrouper {
    let config = GroupingConfig {
        high_traffic_threshold: 12.0,
        medium_traffic_threshold: 6.0,
        max_load_per_group: 20.0,    // 更低的負載限制
        target_groups: 5,            // 更多組，更分散
        balance_tolerance: 0.75,
        allow_mixed_groups: false,   // 不允許混合組
    };
    
    TrafficBasedGrouper::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::SymbolTrafficStats;
    use std::time::Instant;
    
    #[test]
    fn test_traffic_grouping() {
        let grouper = create_hft_optimized_grouper();
        
        let mut traffic_stats = HashMap::new();
        traffic_stats.insert("ETHUSDT".to_string(), SymbolTrafficStats {
            symbol: "ETHUSDT".to_string(),
            message_count: 1000,
            messages_per_second: 21.8,
            average_message_size: 356.5,
            peak_rate: 23.9,
            last_update: Instant::now(),
        });
        
        traffic_stats.insert("BTCUSDT".to_string(), SymbolTrafficStats {
            symbol: "BTCUSDT".to_string(),
            message_count: 800,
            messages_per_second: 14.8,
            average_message_size: 491.5,
            peak_rate: 15.4,
            last_update: Instant::now(),
        });
        
        let grouping = grouper.group_by_traffic(&traffic_stats);
        
        assert!(!grouping.groups.is_empty());
        assert!(grouping.load_balance_ratio > 0.0);
        assert!(grouping.total_load > 0.0);
    }
}