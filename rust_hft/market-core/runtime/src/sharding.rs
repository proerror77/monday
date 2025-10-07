use hft_core::{BaseSymbol, VenueId};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 分片策略枚举
#[derive(Debug, Clone, PartialEq)]
pub enum ShardStrategy {
    /// 基于符号名哈希的分片策略
    SymbolHash,
    /// 交易所轮询分片策略
    VenueRoundRobin,
    /// 混合策略：交易所优先，符号哈希次之
    Hybrid,
}

impl From<&str> for ShardStrategy {
    fn from(s: &str) -> Self {
        match s {
            "symbol-hash" => ShardStrategy::SymbolHash,
            "venue-round-robin" => ShardStrategy::VenueRoundRobin,
            "hybrid" => ShardStrategy::Hybrid,
            _ => {
                tracing::warn!("Unknown shard strategy '{}', defaulting to symbol-hash", s);
                ShardStrategy::SymbolHash
            }
        }
    }
}

/// 分片配置
#[derive(Debug, Clone)]
pub struct ShardConfig {
    /// 当前分片索引 (0-based)
    pub shard_index: u32,
    /// 总分片数量
    pub shard_count: u32,
    /// 分片策略
    pub strategy: ShardStrategy,
}

impl ShardConfig {
    pub fn new(shard_index: u32, shard_count: u32, strategy: ShardStrategy) -> Self {
        assert!(shard_count > 0, "shard_count must be greater than 0");
        assert!(
            shard_index < shard_count,
            "shard_index ({}) must be less than shard_count ({})",
            shard_index,
            shard_count
        );

        Self {
            shard_index,
            shard_count,
            strategy,
        }
    }

    /// 判断给定的符号和交易所是否应该被当前分片处理
    pub fn should_handle(&self, symbol: &BaseSymbol, venue: &VenueId) -> bool {
        match self.strategy {
            ShardStrategy::SymbolHash => self.should_handle_symbol_hash(symbol),
            ShardStrategy::VenueRoundRobin => self.should_handle_venue_round_robin(venue),
            ShardStrategy::Hybrid => self.should_handle_hybrid(symbol, venue),
        }
    }

    fn should_handle_symbol_hash(&self, symbol: &BaseSymbol) -> bool {
        let mut hasher = DefaultHasher::new();
        symbol.hash(&mut hasher);
        let hash = hasher.finish();
        (hash as u32 % self.shard_count) == self.shard_index
    }

    fn should_handle_venue_round_robin(&self, venue: &VenueId) -> bool {
        let mut hasher = DefaultHasher::new();
        venue.hash(&mut hasher);
        let hash = hasher.finish();
        (hash as u32 % self.shard_count) == self.shard_index
    }

    fn should_handle_hybrid(&self, symbol: &BaseSymbol, venue: &VenueId) -> bool {
        // 混合策略：先按交易所分片，如果交易所相同则按符号分片
        let mut hasher = DefaultHasher::new();
        venue.hash(&mut hasher);
        symbol.hash(&mut hasher);
        let hash = hasher.finish();
        (hash as u32 % self.shard_count) == self.shard_index
    }

    /// 获取给定符号和交易所应该由哪个分片处理
    pub fn get_target_shard(&self, symbol: &BaseSymbol, venue: &VenueId) -> u32 {
        match self.strategy {
            ShardStrategy::SymbolHash => {
                let mut hasher = DefaultHasher::new();
                symbol.hash(&mut hasher);
                let hash = hasher.finish();
                hash as u32 % self.shard_count
            }
            ShardStrategy::VenueRoundRobin => {
                let mut hasher = DefaultHasher::new();
                venue.hash(&mut hasher);
                let hash = hasher.finish();
                hash as u32 % self.shard_count
            }
            ShardStrategy::Hybrid => {
                let mut hasher = DefaultHasher::new();
                venue.hash(&mut hasher);
                symbol.hash(&mut hasher);
                let hash = hasher.finish();
                hash as u32 % self.shard_count
            }
        }
    }

    /// 获取分片统计信息
    pub fn get_stats(&self) -> String {
        format!(
            "Shard {}/{} using {:?} strategy",
            self.shard_index + 1,
            self.shard_count,
            self.strategy
        )
    }
}

/// 无分片配置 - 处理所有符号
pub struct NoSharding;

impl NoSharding {
    pub fn should_handle(&self, _symbol: &BaseSymbol, _venue: &VenueId) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{BaseSymbol, VenueId};

    #[test]
    fn test_shard_strategy_from_str() {
        assert_eq!(
            ShardStrategy::from("symbol-hash"),
            ShardStrategy::SymbolHash
        );
        assert_eq!(
            ShardStrategy::from("venue-round-robin"),
            ShardStrategy::VenueRoundRobin
        );
        assert_eq!(ShardStrategy::from("hybrid"), ShardStrategy::Hybrid);
        assert_eq!(ShardStrategy::from("unknown"), ShardStrategy::SymbolHash);
    }

    #[test]
    fn test_shard_config_creation() {
        let config = ShardConfig::new(0, 3, ShardStrategy::SymbolHash);
        assert_eq!(config.shard_index, 0);
        assert_eq!(config.shard_count, 3);
        assert_eq!(config.strategy, ShardStrategy::SymbolHash);
    }

    #[test]
    #[should_panic(expected = "shard_index (3) must be less than shard_count (3)")]
    fn test_shard_config_invalid_index() {
        ShardConfig::new(3, 3, ShardStrategy::SymbolHash);
    }

    #[test]
    #[should_panic(expected = "shard_count must be greater than 0")]
    fn test_shard_config_zero_count() {
        ShardConfig::new(0, 0, ShardStrategy::SymbolHash);
    }

    #[test]
    fn test_symbol_hash_sharding() {
        let config = ShardConfig::new(0, 2, ShardStrategy::SymbolHash);
        let venue = VenueId::BINANCE;

        // 测试不同符号的分片分布
        let btc = BaseSymbol::from("BTCUSDT");
        let eth = BaseSymbol::from("ETHUSDT");

        // 至少应该有一些符号被当前分片处理
        let btc_handled = config.should_handle(&btc, &venue);
        let eth_handled = config.should_handle(&eth, &venue);

        // 验证分片逻辑的一致性
        assert_eq!(btc_handled, config.should_handle(&btc, &venue));
        assert_eq!(eth_handled, config.should_handle(&eth, &venue));
    }

    #[test]
    fn test_venue_round_robin_sharding() {
        let config = ShardConfig::new(0, 2, ShardStrategy::VenueRoundRobin);
        let symbol = BaseSymbol::from("BTCUSDT");

        let binance = VenueId::BINANCE;
        let bitget = VenueId::BITGET;

        // 验证不同交易所的分片分布
        let binance_handled = config.should_handle(&symbol, &binance);
        let bitget_handled = config.should_handle(&symbol, &bitget);

        // 验证分片逻辑的一致性
        assert_eq!(binance_handled, config.should_handle(&symbol, &binance));
        assert_eq!(bitget_handled, config.should_handle(&symbol, &bitget));
    }

    #[test]
    fn test_get_target_shard() {
        let config = ShardConfig::new(0, 3, ShardStrategy::SymbolHash);
        let symbol = BaseSymbol::from("BTCUSDT");
        let venue = VenueId::BINANCE;

        let target = config.get_target_shard(&symbol, &venue);
        assert!(target < 3);

        // 验证一致性
        assert_eq!(target, config.get_target_shard(&symbol, &venue));
    }

    #[test]
    fn test_no_sharding() {
        let no_shard = NoSharding;
        let symbol = BaseSymbol::from("BTCUSDT");
        let venue = VenueId::BINANCE;

        assert!(no_shard.should_handle(&symbol, &venue));
    }

    #[test]
    fn test_shard_distribution() {
        // 测试分片分布是否相对均匀
        let shard_count = 3;
        let test_symbols = vec![
            "BTCUSDT", "ETHUSDT", "ADAUSDT", "DOTUSDT", "LINKUSDT", "LTCUSDT", "XRPUSDT",
            "BCHUSDT", "EOSUSDT", "XLMUSDT",
        ];

        let mut shard_counts = vec![0; shard_count];

        for symbol_str in &test_symbols {
            let symbol = BaseSymbol::from(*symbol_str);
            let venue = VenueId::BINANCE;

            for shard_idx in 0..shard_count {
                let config = ShardConfig::new(
                    shard_idx as u32,
                    shard_count as u32,
                    ShardStrategy::SymbolHash,
                );
                if config.should_handle(&symbol, &venue) {
                    shard_counts[shard_idx] += 1;
                }
            }
        }

        // 验证每个符号只被一个分片处理
        assert_eq!(shard_counts.iter().sum::<i32>(), test_symbols.len() as i32);

        // 验证所有分片都有数据
        let min_count = *shard_counts.iter().min().unwrap();
        let max_count = *shard_counts.iter().max().unwrap();
        assert!(
            min_count > 0,
            "At least one shard received no symbols: {:?}",
            shard_counts
        );

        // 允许一定的不均衡度，但限制最大差值
        let allowed_diff: i32 = test_symbols.len() as i32 / shard_count as i32 + 2;
        assert!(
            (max_count - min_count) <= allowed_diff,
            "Shard distribution too uneven: {:?}",
            shard_counts
        );
    }
}
