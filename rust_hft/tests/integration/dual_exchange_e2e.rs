//! 双交易所端到端集成测试
//! 验证静态分片功能和多场所架构的完整工作流程

use std::time::Duration;
use tokio::time::timeout;
use hft_core::{Symbol, VenueId, BaseSymbol};
use hft_runtime::{SystemBuilder, ShardConfig, ShardStrategy};
use ports::{MarketEvent, ExecutionEvent, Strategy, AccountView, OrderIntent};
use tracing::{info, warn, debug};

/// 简单的测试策略，用于验证多交易所数据流
pub struct DualExchangeTestStrategy {
    name: String,
    symbols: Vec<Symbol>,
    received_events: Vec<(VenueId, String)>, // (venue, symbol)
}

impl DualExchangeTestStrategy {
    pub fn new(name: String, symbols: Vec<Symbol>) -> Self {
        Self {
            name,
            symbols,
            received_events: Vec::new(),
        }
    }

    pub fn get_received_events(&self) -> &[(VenueId, String)] {
        &self.received_events
    }
}

impl Strategy for DualExchangeTestStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) => {
                if let Some(venue) = &snapshot.source_venue {
                    info!("策略 {} 收到来自 {} 的快照: {}",
                          self.name, venue.as_str(), snapshot.symbol.0);
                    self.received_events.push((*venue, snapshot.symbol.0.clone()));
                }
            }
            MarketEvent::Trade(trade) => {
                if let Some(venue) = &trade.source_venue {
                    info!("策略 {} 收到来自 {} 的交易: {}",
                          self.name, venue.as_str(), trade.symbol.0);
                    self.received_events.push((*venue, trade.symbol.0.clone()));
                }
            }
            MarketEvent::Bar(bar) => {
                if let Some(venue) = &bar.source_venue {
                    info!("策略 {} 收到来自 {} 的K线: {}",
                          self.name, venue.as_str(), bar.symbol.0);
                    self.received_events.push((*venue, bar.symbol.0.clone()));
                }
            }
            _ => {}
        }
        Vec::new() // 测试策略不生成订单
    }

    fn on_execution_event(&mut self, _event: &ExecutionEvent, _account: &AccountView) -> Vec<OrderIntent> {
        Vec::new()
    }
}

/// 测试配置：双交易所 BTCUSDT
const TEST_CONFIG_YAML: &str = r#"
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10
  flip_policy: OnUpdate

venues:
  - name: "binance"
    venue_type: "Binance"
    api_key: "test_key"
    secret: "test_secret"
    capabilities:
      ws_order_placement: false
      snapshot_crc: false
      all_in_one_topics: true
      private_ws_heartbeat: true

  - name: "bitget"
    venue_type: "Bitget"
    api_key: "test_key"
    secret: "test_secret"
    passphrase: "test_passphrase"
    capabilities:
      ws_order_placement: true
      snapshot_crc: false
      all_in_one_topics: true
      private_ws_heartbeat: true

strategies: []

risk:
  global_position_limit: 100000
  global_notional_limit: 1000000
  max_daily_trades: 1000
  max_orders_per_second: 10
  staleness_threshold_us: 3000
"#;

#[tokio::test]
async fn test_dual_exchange_sharding_symbol_hash() {
    tracing_subscriber::fmt::init();

    info!("开始双交易所符号哈希分片测试");

    // 写入临时配置文件
    let config_path = "/tmp/dual_exchange_test.yaml";
    std::fs::write(config_path, TEST_CONFIG_YAML).expect("无法写入测试配置");

    // 测试符号哈希分片策略 (shard 0/2)
    let shard_config = ShardConfig::new(0, 2, ShardStrategy::SymbolHash);

    let builder = SystemBuilder::from_yaml(config_path)
        .expect("无法加载配置")
        .with_sharding(shard_config)
        .auto_register_adapters();

    // 注册测试策略
    let test_symbols = vec![
        Symbol("BINANCE:BTCUSDT".to_string()),
        Symbol("BITGET:BTCUSDT".to_string()),
    ];
    let mut strategy = DualExchangeTestStrategy::new("dual_test".to_string(), test_symbols.clone());

    let mut system = builder.build();
    system.register_strategy_boxed(Box::new(strategy));

    // 启动系统并运行短时间
    tokio::spawn(async move {
        if let Err(e) = system.start().await {
            warn!("系统启动失败: {}", e);
        }
    });

    // 等待数据流建立
    tokio::time::sleep(Duration::from_millis(500)).await;

    info!("双交易所符号哈希分片测试完成");

    // 清理
    let _ = std::fs::remove_file(config_path);
}

#[tokio::test]
async fn test_dual_exchange_sharding_venue_round_robin() {
    tracing_subscriber::fmt::init();

    info!("开始双交易所场所轮询分片测试");

    // 写入临时配置文件
    let config_path = "/tmp/dual_exchange_venue_test.yaml";
    std::fs::write(config_path, TEST_CONFIG_YAML).expect("无法写入测试配置");

    // 测试场所轮询分片策略 (shard 1/2 - 应该处理 BITGET)
    let shard_config = ShardConfig::new(1, 2, ShardStrategy::VenueRoundRobin);

    let builder = SystemBuilder::from_yaml(config_path)
        .expect("无法加载配置")
        .with_sharding(shard_config)
        .auto_register_adapters();

    let mut system = builder.build();

    // 启动系统
    tokio::spawn(async move {
        if let Err(e) = system.start().await {
            warn!("系统启动失败: {}", e);
        }
    });

    // 等待系统稳定
    tokio::time::sleep(Duration::from_millis(300)).await;

    info!("双交易所场所轮询分片测试完成");

    // 清理
    let _ = std::fs::remove_file(config_path);
}

#[tokio::test]
async fn test_sharding_distribution_correctness() {
    info!("开始测试分片分布正确性");

    // 测试符号哈希分布
    let symbols = vec![
        BaseSymbol("BTCUSDT".to_string()),
        BaseSymbol("ETHUSDT".to_string()),
        BaseSymbol("ADAUSDT".to_string()),
        BaseSymbol("BNBUSDT".to_string()),
    ];

    let venues = vec![VenueId::BINANCE, VenueId::BITGET];

    // 测试 2 分片的分布
    for shard_count in [2, 3] {
        info!("测试 {} 分片分布", shard_count);

        let mut shard_assignments = vec![Vec::new(); shard_count];

        for shard_index in 0..shard_count {
            let config = ShardConfig::new(shard_index, shard_count, ShardStrategy::SymbolHash);

            for symbol in &symbols {
                for venue in &venues {
                    if config.should_handle(symbol, venue) {
                        shard_assignments[shard_index].push((symbol.0.clone(), *venue));
                    }
                }
            }
        }

        // 验证每个 (symbol, venue) 组合只被一个分片处理
        let mut all_assignments = std::collections::HashSet::new();
        for (shard_idx, assignments) in shard_assignments.iter().enumerate() {
            info!("分片 {} 处理: {:?}", shard_idx, assignments);

            for assignment in assignments {
                assert!(all_assignments.insert(assignment.clone()),
                       "重复分配: {:?}", assignment);
            }
        }

        // 验证所有 (symbol, venue) 组合都被分配
        let expected_count = symbols.len() * venues.len();
        assert_eq!(all_assignments.len(), expected_count,
                  "分配数量不匹配: expected {}, got {}", expected_count, all_assignments.len());
    }

    info!("分片分布正确性测试完成");
}

#[tokio::test]
async fn test_cross_exchange_arbitrage_setup() {
    info!("开始测试跨交易所套利设置");

    // 写入包含套利策略的配置
    let arbitrage_config = r#"
engine:
  queue_capacity: 1024
  stale_us: 3000
  top_n: 10
  flip_policy: OnUpdate

venues:
  - name: "binance"
    venue_type: "Binance"
    api_key: "test_key"
    secret: "test_secret"
  - name: "bitget"
    venue_type: "Bitget"
    api_key: "test_key"
    secret: "test_secret"
    passphrase: "test_passphrase"

strategies:
  - name: "cross_venue_arb"
    strategy_type: "Arbitrage"
    symbols: ["BTCUSDT"]
    venues: ["BINANCE", "BITGET"]
    params:
      min_spread_bps: 5
      max_position: 1.0

risk:
  global_position_limit: 50000
  global_notional_limit: 500000
  max_daily_trades: 500
  max_orders_per_second: 5
  staleness_threshold_us: 2000
"#;

    let config_path = "/tmp/arbitrage_test.yaml";
    std::fs::write(config_path, arbitrage_config).expect("无法写入套利测试配置");

    // 不使用分片（单实例处理所有数据）
    let builder = SystemBuilder::from_yaml(config_path)
        .expect("无法加载套利配置")
        .auto_register_adapters();

    let mut system = builder.build();

    // 启动系统
    let system_handle = tokio::spawn(async move {
        if let Err(e) = system.start().await {
            warn!("套利测试系统启动失败: {}", e);
        }
    });

    // 让系统运行一段时间
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 停止系统
    system_handle.abort();

    info!("跨交易所套利设置测试完成");

    // 清理
    let _ = std::fs::remove_file(config_path);
}

/// 测试混合分片策略的负载均衡
#[tokio::test]
async fn test_hybrid_sharding_load_balance() {
    info!("开始测试混合分片策略负载均衡");

    let symbols = vec![
        BaseSymbol("BTCUSDT".to_string()),
        BaseSymbol("ETHUSDT".to_string()),
        BaseSymbol("ADAUSDT".to_string()),
        BaseSymbol("BNBUSDT".to_string()),
        BaseSymbol("DOTUSDT".to_string()),
        BaseSymbol("LINKUSDT".to_string()),
    ];

    let venues = vec![VenueId::BINANCE, VenueId::BITGET];
    let shard_count = 3;

    let mut load_per_shard = vec![0; shard_count];

    for shard_index in 0..shard_count {
        let config = ShardConfig::new(shard_index, shard_count, ShardStrategy::Hybrid);

        for symbol in &symbols {
            for venue in &venues {
                if config.should_handle(symbol, venue) {
                    load_per_shard[shard_index] += 1;
                }
            }
        }
    }

    info!("混合分片负载分布: {:?}", load_per_shard);

    // 验证负载相对均衡（允许 ±1 的差异）
    let min_load = *load_per_shard.iter().min().unwrap();
    let max_load = *load_per_shard.iter().max().unwrap();

    assert!(max_load - min_load <= 2,
           "负载不均衡过大: min={}, max={}, diff={}",
           min_load, max_load, max_load - min_load);

    // 验证总负载等于总的 (symbol, venue) 组合数
    let total_load: usize = load_per_shard.iter().sum();
    assert_eq!(total_load, symbols.len() * venues.len());

    info!("混合分片策略负载均衡测试完成");
}