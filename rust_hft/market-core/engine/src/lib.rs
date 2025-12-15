//! Single-writer engine loop（骨架）
//! - Ingest bounded rings from I/O
//! - Aggregate L2 TopN and Bars
//! - Publish snapshots via snapshot crate
//! - Route: Strategy -> Risk -> Execution

pub mod adapter_bridge;
pub mod aggregation;
pub mod dataflow;
pub mod execution_queues;
pub mod execution_worker;
pub mod latency_monitor;

use aggregation::{AggregationEngine, MarketView};
use dataflow::{EventConsumer, IngestionConfig};
use execution_queues::EngineQueues;
use hft_core::{
    now_micros, HftError, HftResult, LatencyStage, LatencyTracker, OrderType, Side, Symbol, VenueId,
};
use latency_monitor::{LatencyMonitor, LatencyMonitorConfig};
use ports::Trade as MarketTrade;
use ports::{
    AccountView, BoxStream, ExecutionClient, ExecutionEvent, OrderManager, PortfolioManager,
    Strategy, VenueSpec,
};
use rustc_hash::FxHashMap;
use snapshot::SnapshotContainer;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Notify;
use tracing::{debug, error, info, trace, warn};

// 重新導出關鍵類型
pub use adapter_bridge::{AdapterBridge, AdapterBridgeConfig};
pub use dataflow::{BackpressurePolicy, BackpressureStatus, FlipPolicy};
pub use execution_queues::{create_execution_queues, ExecutionQueueConfig, WorkerQueues};
pub use execution_worker::{spawn_execution_worker, ExecutionWorker, ExecutionWorkerConfig};

/// 交易狀態模式（由 Sentinel 控制）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TradingMode {
    /// 正常交易
    #[default]
    Normal,
    /// 降頻模式 - 減少交易頻率
    Degraded,
    /// 暫停交易 - 不生成新訂單
    Paused,
    /// 緊急模式 - 準備平倉
    Emergency,
}

/// 引擎運行統計（內部狀態）
#[derive(Debug, Default)]
pub struct EngineStats {
    pub cycle_count: u64,
    pub is_running: bool,
    pub execution_events_processed: u64,
    pub orders_submitted: u64,
    pub orders_ack: u64,
    pub orders_filled: u64,
    pub orders_rejected: u64,
    pub orders_canceled: u64,
    // P3: 指標增強
    pub market_events_dropped: u64,
    pub intents_dropped: u64,
    pub snapshot_publish_failed: u64,
    // Sentinel 控制
    pub trading_mode: TradingMode,
    /// 引擎啟動時間 (微秒時間戳)
    pub start_time_us: u64,
}

/// 事件廣播器集合
pub struct EventBroadcasters {
    pub exec_event_tx: broadcast::Sender<ports::ExecutionEvent>,
    pub market_trade_tx: broadcast::Sender<MarketTrade>,
    pub market_event_tx: broadcast::Sender<ports::MarketEvent>,
}

/// 引擎運行時配置
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub ingestion: IngestionConfig,
    pub max_events_per_cycle: u32,
    pub aggregation_symbols: Vec<Symbol>,
    pub latency_monitor: LatencyMonitorConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ingestion: IngestionConfig::default(),
            max_events_per_cycle: 100,
            aggregation_symbols: vec![],
            latency_monitor: LatencyMonitorConfig::default(),
        }
    }
}

/// 單寫入者引擎
pub struct Engine {
    /// 配置
    config: EngineConfig,
    /// 事件消費者 (從 adapters 接收)
    event_consumers: Vec<EventConsumer>,
    /// 聚合引擎
    aggregation_engine: AggregationEngine,
    /// 市場快照發佈容器
    market_snapshots: SnapshotContainer<MarketView>,
    /// 帳戶快照發佈容器
    account_snapshots: SnapshotContainer<AccountView>,
    /// 註冊的策略
    strategies: Vec<Box<dyn Strategy>>,
    /// 風控管理器（可選）
    risk_manager: Option<Box<dyn ports::RiskManager>>,
    /// VenueSpec 映射（用於場所特定的風控）
    venue_specs: HashMap<VenueId, VenueSpec>,
    /// 註冊的執行客戶端
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    /// 執行回報流（與 execution_clients 對應）
    #[allow(dead_code)]
    execution_streams: Vec<BoxStream<ExecutionEvent>>,
    /// OMS 核心 - 訂單生命週期管理（通過 trait 注入）
    order_manager: Option<Box<dyn OrderManager>>,
    /// Portfolio 核心 - 會計與資金管理（通過 trait 注入）
    portfolio_manager: Option<Box<dyn PortfolioManager>>,
    /// 执行队列系统 (SPSC)
    execution_queues: Option<EngineQueues>,
    /// 運行統計
    stats: EngineStats,
    /// 暫存的市場事件（從聚合引擎產生）
    pending_market_events: Vec<ports::MarketEvent>,
    /// 復用的策略意圖工作緩衝，減少熱路徑 Vec 分配
    intents_work_buf: Vec<ports::OrderIntent>,
    /// 復用的執行事件緩衝，避免每 tick 分配
    execution_events_buf: Vec<ExecutionEvent>,
    /// 事件驱动唤醒通知器
    wakeup_notify: Arc<Notify>,
    /// 訂單提交時間（用於 Ack/Fill 延遲計算）
    order_submit_ts: FxHashMap<hft_core::OrderId, u64>,
    /// 最近處理的市場事件時間戳（用於端到端延遲計算）
    recent_market_event_timestamp: Option<u64>,
    /// 延遲監控器 - 統一收集所有階段延遲
    latency_monitor: LatencyMonitor,
    /// 事件廣播器
    broadcasters: EventBroadcasters,
    /// 策略到場所的映射（用於單場策略事件過濾）
    strategy_venue_mapping: FxHashMap<String, VenueId>,
    /// 策略實例 ID 列表（與 strategies Vec 順序對應，用於事件過濾）
    strategy_instance_ids: Vec<String>,
    /// 已禁用的策略索引集合（索引對應 strategies Vec）
    disabled_strategy_indices: std::collections::HashSet<usize>,
    /// 订单到帳戶映射（Phase 1）
    order_account_map: FxHashMap<hft_core::OrderId, hft_core::AccountId>,
    /// 策略到帳戶映射（Phase 1：由 runtime 配置）
    strategy_account_mapping: FxHashMap<String, hft_core::AccountId>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        // 創建初始空的市場視圖
        let initial_market_view = MarketView {
            orderbooks: FxHashMap::default(),
            arbitrage_opportunities: Vec::new(),
            timestamp: 0,
            version: 0,
        };

        // 創建初始空的帳戶視圖
        let initial_account_view = AccountView::default();

        let latency_monitor = LatencyMonitor::new(config.latency_monitor.clone());
        let (exec_event_tx, _rx) = broadcast::channel(1024);
        let (market_trade_tx, _rx_trade) = broadcast::channel(4096);
        let (market_event_tx, _rx_mev) = broadcast::channel(4096);
        let broadcasters = EventBroadcasters {
            exec_event_tx,
            market_trade_tx,
            market_event_tx,
        };

        Self {
            config,
            event_consumers: Vec::new(),
            aggregation_engine: AggregationEngine::new(),
            market_snapshots: SnapshotContainer::new(initial_market_view),
            account_snapshots: SnapshotContainer::new(initial_account_view),
            strategies: Vec::new(),
            risk_manager: None,
            venue_specs: VenueSpec::build_default_venue_specs(),
            execution_clients: Vec::new(),
            execution_streams: Vec::new(),
            order_manager: None,
            portfolio_manager: None,
            execution_queues: None,
            stats: EngineStats {
                cycle_count: 0,
                is_running: true, // 默認為運行狀態，讓 tick() 可以正常運作
                execution_events_processed: 0,
                orders_submitted: 0,
                orders_ack: 0,
                orders_filled: 0,
                orders_rejected: 0,
                orders_canceled: 0,
                market_events_dropped: 0,
                intents_dropped: 0,
                snapshot_publish_failed: 0,
                trading_mode: TradingMode::Normal,
                start_time_us: now_micros(),
            },
            pending_market_events: Vec::new(),
            intents_work_buf: Vec::new(),
            execution_events_buf: Vec::new(),
            wakeup_notify: Arc::new(Notify::new()),
            order_submit_ts: FxHashMap::default(),
            recent_market_event_timestamp: None,
            latency_monitor,
            broadcasters,
            strategy_venue_mapping: FxHashMap::default(),
            strategy_instance_ids: Vec::new(),
            disabled_strategy_indices: std::collections::HashSet::new(),
            order_account_map: FxHashMap::default(),
            strategy_account_mapping: FxHashMap::default(),
        }
    }

    /// 设置执行队列系统（Runtime 设置）
    pub fn set_execution_queues(&mut self, queues: EngineQueues) {
        self.execution_queues = Some(queues);
    }

    /// 設置訂單管理器（依賴注入）
    pub fn set_order_manager(&mut self, manager: Box<dyn OrderManager>) {
        self.order_manager = Some(manager);
    }

    /// 設置 Portfolio 管理器（依賴注入）
    pub fn set_portfolio_manager(&mut self, manager: Box<dyn PortfolioManager>) {
        self.portfolio_manager = Some(manager);
    }

    /// 設置策略到場所的映射（用於單場策略事件過濾）
    pub fn set_strategy_venue_mapping(&mut self, mapping: HashMap<String, VenueId>) {
        self.strategy_venue_mapping = mapping.into_iter().collect();
    }

    /// 設置策略到帳戶的映射（Phase 1 多帳戶）
    pub fn set_strategy_account_mapping(&mut self, mapping: HashMap<String, hft_core::AccountId>) {
        self.strategy_account_mapping = mapping.into_iter().collect();
    }

    /// 處理市場事件給策略（測試用）：依賴策略的 venue_scope 與 runtime 的策略→場館映射
    #[cfg(test)]
    pub fn process_market_event_for_strategies(
        &mut self,
        event: &ports::MarketEvent,
        account: &AccountView,
    ) {
        for (strategy_idx, strategy) in self.strategies.iter_mut().enumerate() {
            let unknown_id = "unknown".to_string();
            let strategy_instance_id = self
                .strategy_instance_ids
                .get(strategy_idx)
                .unwrap_or(&unknown_id);

            // 事件範疇過濾（與 run_strategies_sync 保持一致）：
            let mut should_process = true;
            if matches!(strategy.venue_scope(), ports::VenueScope::Single) {
                // 取得事件來源場域
                let event_venue = match event {
                    ports::MarketEvent::Snapshot(snapshot) => snapshot.source_venue,
                    ports::MarketEvent::Update(update) => update.source_venue,
                    ports::MarketEvent::Trade(trade) => trade.source_venue,
                    ports::MarketEvent::Bar(bar) => bar.source_venue,
                    _ => None,
                };
                if let Some(&expected_venue) = self.strategy_venue_mapping.get(strategy_instance_id)
                {
                    if let Some(ev_venue) = event_venue {
                        should_process = expected_venue == ev_venue;
                    }
                }
            }
            if should_process {
                let _ = strategy.on_market_event(event, account);
            }
        }
    }

    /// 移出执行客户端给 worker 使用 (仅调用一次)
    pub fn take_execution_clients(&mut self) -> Vec<Box<dyn ExecutionClient>> {
        std::mem::take(&mut self.execution_clients)
    }

    /// 获取引擎唤醒通知器的克隆，用于外部触发引擎唤醒
    pub fn get_wakeup_notify(&self) -> Arc<Notify> {
        self.wakeup_notify.clone()
    }

    /// 唤醒引擎（触发新的 tick 周期）
    pub fn wake_engine(&self) {
        self.wakeup_notify.notify_one();
    }

    /// 獲取延遲監控器的引用
    pub fn latency_monitor(&self) -> &LatencyMonitor {
        &self.latency_monitor
    }

    /// 設置 VenueSpec 映射
    pub fn set_venue_specs(&mut self, venue_specs: HashMap<VenueId, VenueSpec>) {
        self.venue_specs = venue_specs;
    }

    /// 添加單個 VenueSpec
    pub fn add_venue_spec(&mut self, venue_id: VenueId, spec: VenueSpec) {
        self.venue_specs.insert(venue_id, spec);
    }

    /// 導出 OMS 狀態（供恢復/持久化使用）
    pub fn export_oms_state(&self) -> HashMap<hft_core::OrderId, ports::OrderRecord> {
        self.order_manager
            .as_ref()
            .map(|om| om.export_state())
            .unwrap_or_default()
    }

    /// 取得指定策略的未結訂單 (order_id, symbol) 配對，供外部控制/撤單使用
    pub fn open_order_pairs_by_strategy(
        &self,
        strategy_id: &str,
    ) -> Vec<(hft_core::OrderId, hft_core::Symbol)> {
        self.order_manager
            .as_ref()
            .map(|om| om.open_order_pairs_by_strategy(strategy_id))
            .unwrap_or_default()
    }

    /// 導入 OMS 狀態（供恢復/持久化使用）
    pub fn import_oms_state(&mut self, state: HashMap<hft_core::OrderId, ports::OrderRecord>) {
        if let Some(om) = &mut self.order_manager {
            om.import_state(state);
        }
    }

    /// 導出 Portfolio 狀態（供恢復/持久化使用）
    pub fn export_portfolio_state(&self) -> ports::PortfolioState {
        self.portfolio_manager
            .as_ref()
            .map(|pm| pm.export_state())
            .unwrap_or_else(|| ports::PortfolioState {
                account_view: AccountView::default(),
                order_meta: HashMap::new(),
                market_prices: HashMap::new(),
                processed_fill_ids: HashMap::new(),
            })
    }

    /// 導入 Portfolio 狀態（供恢復/持久化使用）
    pub fn import_portfolio_state(&mut self, state: ports::PortfolioState) {
        if let Some(pm) = &mut self.portfolio_manager {
            pm.import_state(state);
        }
    }

    /// 獲取所有階段的延遲統計數據
    pub fn get_latency_stats(
        &self,
    ) -> std::collections::HashMap<hft_core::LatencyStage, hft_core::latency::LatencyStageStats>
    {
        self.latency_monitor.get_all_stats()
    }

    /// 訂閱執行事件（供外部追蹤取消等回覆）
    pub fn subscribe_execution_events(&self) -> broadcast::Receiver<ports::ExecutionEvent> {
        self.broadcasters.exec_event_tx.subscribe()
    }

    /// 訂閱市場成交事件（供外部導出/監控）
    pub fn subscribe_market_trades(&self) -> broadcast::Receiver<MarketTrade> {
        self.broadcasters.market_trade_tx.subscribe()
    }
    /// 訂閱市場事件（供外部導出/監控）
    pub fn subscribe_market_events(&self) -> broadcast::Receiver<ports::MarketEvent> {
        self.broadcasters.market_event_tx.subscribe()
    }

    /// 同步延遲統計到 Prometheus（可選）
    #[cfg(feature = "metrics")]
    pub fn sync_latency_metrics_to_prometheus(&self) {
        let latency_stats = self.latency_monitor.get_all_stats();
        if !latency_stats.is_empty() {
            infra_metrics::MetricsRegistry::global().update_from_latency_monitor(&latency_stats);
            debug!("同步了 {} 個延遲統計到 Prometheus", latency_stats.len());
        }
        // 同步引擎統計（以 Gauge）
        let s = self.get_statistics();
        let export = infra_metrics::EngineStatisticsExport {
            cycle_count: s.cycle_count,
            execution_events_processed: s.execution_events_processed,
            orders_submitted: s.orders_submitted,
            orders_ack: s.orders_ack,
            orders_filled: s.orders_filled,
            orders_rejected: s.orders_rejected,
            orders_canceled: s.orders_canceled,
        };
        infra_metrics::MetricsRegistry::global().update_engine_statistics(&export);
    }

    #[cfg(not(feature = "metrics"))]
    pub fn sync_latency_metrics_to_prometheus(&self) {
        // No-op when metrics disabled
    }

    /// 註冊事件消費者 (對應一個 market adapter)
    pub fn register_event_consumer(&mut self, mut consumer: EventConsumer) {
        // 为消费者设置引擎唤醒通知器
        consumer.set_engine_notify(self.wakeup_notify.clone());
        self.event_consumers.push(consumer);
    }

    /// 創建配對的 EventIngester 和 EventConsumer (用於零拷貝適配器)
    pub fn create_event_ingester_pair(&mut self) -> Arc<std::sync::Mutex<dataflow::EventIngester>> {
        let config = dataflow::IngestionConfig {
            queue_capacity: self.config.ingestion.queue_capacity,
            stale_threshold_us: self.config.ingestion.stale_threshold_us,
            ..Default::default()
        };

        let (mut ingester, mut consumer) = dataflow::EventIngester::new(config);

        // 將引擎喚醒通知器掛載到攝取器與消費者
        let notify = self.wakeup_notify.clone();
        ingester.set_engine_notify(notify.clone());
        consumer.set_engine_notify(notify);

        // 註冊消費者到引擎
        self.event_consumers.push(consumer);

        // 返回攝取器供零拷貝適配器使用
        Arc::new(std::sync::Mutex::new(ingester))
    }

    /// 註冊策略
    pub fn register_strategy<S: Strategy + 'static>(&mut self, s: S) {
        // 在註冊策略時記錄其實例 ID（而不是類型名稱）
        let instance_id = self.extract_strategy_instance_id(&s);
        self.strategy_instance_ids.push(instance_id);
        self.strategies.push(Box::new(s));
    }

    pub fn register_strategy_boxed(&mut self, s: Box<dyn Strategy>) {
        // 在註冊策略時記錄其實例 ID（而不是類型名稱）
        let instance_id = self.extract_strategy_instance_id(s.as_ref());
        self.strategy_instance_ids.push(instance_id);
        self.strategies.push(s);
    }

    pub fn strategy_instance_ids(&self) -> Vec<String> {
        self.strategy_instance_ids.clone()
    }

    /// 通过策略 ID 获取策略的不可变引用
    #[allow(clippy::borrowed_box)]
    pub fn get_strategy_by_id(&self, strategy_id: &str) -> Option<&Box<dyn Strategy>> {
        let index = self
            .strategy_instance_ids
            .iter()
            .position(|id| id == strategy_id)?;
        self.strategies.get(index)
    }

    /// 通过策略 ID 获取策略的可变引用
    #[allow(clippy::borrowed_box)]
    pub fn get_strategy_mut_by_id(&mut self, strategy_id: &str) -> Option<&mut Box<dyn Strategy>> {
        let index = self
            .strategy_instance_ids
            .iter()
            .position(|id| id == strategy_id)?;
        self.strategies.get_mut(index)
    }

    /// 设置策略启用/禁用状态
    pub fn set_strategy_enabled(&mut self, strategy_id: &str, enabled: bool) -> HftResult<()> {
        let index = self
            .strategy_instance_ids
            .iter()
            .position(|id| id == strategy_id)
            .ok_or_else(|| HftError::Config(format!("策略 {} 不存在", strategy_id)))?;

        if enabled {
            // 从禁用列表中移除
            self.disabled_strategy_indices.remove(&index);
        } else {
            // 添加到禁用列表
            self.disabled_strategy_indices.insert(index);
        }

        Ok(())
    }

    pub fn replace_strategy(
        &mut self,
        index: usize,
        mut strategy: Box<dyn Strategy>,
    ) -> HftResult<()> {
        if index >= self.strategies.len() {
            return Err(HftError::Config(format!(
                "策略索引超出範圍: {} (總數 {})",
                index,
                self.strategies.len()
            )));
        }

        strategy.initialize()?;
        let instance_id = self.extract_strategy_instance_id(strategy.as_ref());

        if let Some(old) = self.strategies.get_mut(index) {
            let _ = old.shutdown();
        }

        self.strategies[index] = strategy;
        self.strategy_instance_ids[index] = instance_id;
        Ok(())
    }

    /// 提取策略實例 ID（用於事件過濾映射）：優先使用 Strategy::id()
    fn extract_strategy_instance_id<S: Strategy + ?Sized>(&self, strategy: &S) -> String {
        strategy.id().to_string()
    }

    /// 註冊風控管理器
    pub fn register_risk_manager<R: ports::RiskManager + 'static>(&mut self, r: R) {
        self.risk_manager = Some(Box::new(r));
    }

    pub fn register_risk_manager_boxed(&mut self, r: Box<dyn ports::RiskManager>) {
        self.risk_manager = Some(r);
    }

    /// 註冊執行客戶端
    pub fn register_execution_client<E: ExecutionClient + 'static>(&mut self, e: E) {
        self.execution_clients.push(Box::new(e));
    }

    pub fn register_execution_client_boxed(&mut self, e: Box<dyn ExecutionClient>) {
        self.execution_clients.push(e);
    }

    /// 獲取當前市場視圖快照
    pub fn get_market_view(&self) -> Arc<MarketView> {
        self.market_snapshots.load()
    }

    /// 獲取當前帳戶視圖快照
    pub fn get_account_view(&self) -> Arc<AccountView> {
        self.account_snapshots.load()
    }

    /// 獲取風控指標
    pub fn get_risk_metrics(&self) -> Option<ports::RiskMetrics> {
        self.risk_manager.as_ref().map(|rm| rm.risk_metrics())
    }

    /// 動態更新風控配置
    pub fn update_risk_config(
        &mut self,
        update: ports::RiskConfigUpdate,
    ) -> Result<(), HftError> {
        if let Some(rm) = &mut self.risk_manager {
            rm.update_config(update)
        } else {
            Err(HftError::Config("未註冊風控管理器".to_string()))
        }
    }

    /// 獲取當前風控配置快照
    pub fn get_risk_config_snapshot(&self) -> Option<ports::RiskConfigSnapshot> {
        self.risk_manager.as_ref().map(|rm| rm.get_config_snapshot())
    }

    /// 獲取市場快照讀取者
    pub fn market_reader(&self) -> Arc<dyn snapshot::SnapshotReader<MarketView>> {
        self.market_snapshots.reader()
    }

    /// 獲取帳戶快照讀取者
    pub fn account_reader(&self) -> Arc<dyn snapshot::SnapshotReader<AccountView>> {
        self.account_snapshots.reader()
    }

    /// 提交訂單意圖到執行隊列（用於 dry-run 測試）
    pub fn submit_order_intent(&mut self, intent: ports::OrderIntent) -> Result<(), HftError> {
        if let Some(queues) = &mut self.execution_queues {
            match queues.send_intent(intent) {
                Ok(()) => {
                    debug!("成功提交訂單意圖到執行隊列");
                    Ok(())
                }
                Err(rejected_intent) => {
                    warn!(
                        "執行隊列滿載，訂單意圖被拒絕: {} {}",
                        rejected_intent.symbol.as_str(),
                        rejected_intent.quantity.0
                    );
                    Err(HftError::Execution("執行隊列滿載".to_string()))
                }
            }
        } else {
            warn!("執行隊列未初始化，無法提交訂單意圖");
            Err(HftError::Execution("執行隊列未初始化".to_string()))
        }
    }

    /// 單次事件循環
    pub fn tick(&mut self) -> Result<EngineTickResult, HftError> {
        self.stats.cycle_count += 1;
        let mut result = EngineTickResult::default();

        // 只在前 100 個循環和有事件時記錄
        if self.stats.cycle_count <= 100 || self.stats.cycle_count.is_multiple_of(1000) {
            debug!("引擎 tick #{}", self.stats.cycle_count);
        }

        // 階段 1: 消費市場事件
        let mut total_events = 0;
        let mut should_flip = false;

        for (idx, consumer) in self.event_consumers.iter_mut().enumerate() {
            let mut consumer_events = 0;
            let consumer_should_flip = consumer.consume_events(|event| {
                consumer_events += 1;
                total_events += 1;
                result.events_processed += 1;

                // 記錄接入階段延遲（從事件原始時間戳到現在）
                let ingestion_latency = now_micros().saturating_sub(event.tracker.origin_time);
                self.latency_monitor
                    .record_latency(LatencyStage::Ingestion, ingestion_latency);

                // 記錄聚合階段開始（零拷貝：直接使用隊列彈出的事件所有權）
                let mut tracked_event = event;
                tracked_event.record_stage(hft_core::latency::LatencyStage::Aggregation);

                // 更新市場事件時間戳供端到端延遲計算
                self.recent_market_event_timestamp = Some(tracked_event.tracker.origin_time);

                // 日誌降噪：逐事件日誌改為 debug 級別
                let (event_kind, symbol) = match &tracked_event.event {
                    ports::MarketEvent::Bar(bar) => ("bar", bar.symbol.as_str()),
                    ports::MarketEvent::Trade(trade) => ("trade", trade.symbol.as_str()),
                    ports::MarketEvent::Snapshot(snap) => ("snapshot", snap.symbol.as_str()),
                    _ => ("other", ""),
                };

                debug!(
                    cycle = self.stats.cycle_count,
                    seq = total_events,
                    consumer = idx,
                    event_kind,
                    symbol,
                    "引擎 tick 處理事件"
                );

                // 若為 Trade 事件，廣播一份給外部訂閱方（最佳努力）
                if let ports::MarketEvent::Trade(t) = &tracked_event.event {
                    if self.broadcasters.market_trade_tx.receiver_count() > 0 {
                        let _ = self.broadcasters.market_trade_tx.send(t.clone());
                    }
                }
                // 廣播原始市場事件（含 Snapshot/Update/Trade/Bar），供外部導出或監控
                if self.broadcasters.market_event_tx.receiver_count() > 0 {
                    #[allow(unused_must_use)]
                    {
                        let _ = self
                            .broadcasters
                            .market_event_tx
                            .send(tracked_event.event.clone());
                    }
                }

                // 處理事件 -> 聚合引擎
                let aggregation_start = now_micros();
                let before_len = self.pending_market_events.len();
                match self
                    .aggregation_engine
                    .handle_event_into(tracked_event.event, &mut self.pending_market_events)
                {
                    Ok(()) => {
                        let event_count = self.pending_market_events.len() - before_len;
                        if event_count > 0 {
                            debug!("聚合引擎生成 {} 個事件", event_count);
                        }

                        // 記錄聚合階段延遲到統一監控器
                        let aggregation_latency = now_micros().saturating_sub(aggregation_start);
                        self.latency_monitor
                            .record_latency(LatencyStage::Aggregation, aggregation_latency);

                        // 記錄聚合延遲到 Prometheus
                        #[cfg(feature = "metrics")]
                        infra_metrics::MetricsRegistry::global()
                            .record_aggregation_latency(aggregation_latency as f64);

                        // 如果有新事件產生，觸發快照發佈
                        !self.pending_market_events.is_empty()
                    }
                    Err(e) => {
                        warn!("聚合引擎處理事件失敗: {}", e);
                        false
                    }
                }
            });

            // 日誌降噪：批量統計改為 debug，只有當有事件時才記錄
            if consumer_events > 0 {
                debug!("消費者 {} 處理了 {} 個事件", idx, consumer_events);
            }

            should_flip = should_flip || consumer_should_flip;

            // 批處理限制
            if total_events >= self.config.max_events_per_cycle {
                break;
            }
        }

        result.events_total = total_events;

        // 階段 2: 處理執行事件 (來自执行队列) - 非阻塞批量接收
        let mut exec_processed = 0u32;
        let mut execution_events_buf = std::mem::take(&mut self.execution_events_buf);
        execution_events_buf.clear();
        if let Some(queues) = &mut self.execution_queues {
            queues.receive_events_into(&mut execution_events_buf);
            for ev in &execution_events_buf {
                self.handle_execution_event(ev)?;
                exec_processed += 1;
                self.stats.execution_events_processed += 1;
            }
        }
        result.execution_events_processed = exec_processed;
        execution_events_buf.clear();
        self.execution_events_buf = execution_events_buf;

        // 階段 3: 檢查是否需要發佈快照
        if should_flip
            || self.aggregation_engine.should_publish_snapshot()
            || result.execution_events_processed > 0
        {
            self.publish_market_snapshot(&mut result)?;
            self.publish_account_snapshot(&mut result)?;
        }

        // 階段 4: 策略決策 (基於最新快照)
        if result.snapshot_published {
            // Note: run_strategies simplified to sync for tick() compatibility
            self.run_strategies_sync(&mut result)?;
        }

        // 🔥 降低日誌等級：只在每 100 個 tick 或有大量活動時記錄（避免洪流）
        let should_log = self.stats.cycle_count.is_multiple_of(100)
            || total_events > 100
            || result.orders_generated > 10;

        if should_log && (total_events > 0 || exec_processed > 0 || result.orders_generated > 0) {
            debug!(
                "Tick #{}: {} market events, {} exec events, {} orders, snapshot: {}",
                self.stats.cycle_count,
                total_events,
                exec_processed,
                result.orders_generated,
                result.snapshot_published
            );
        } else if total_events > 0 || exec_processed > 0 || result.orders_generated > 0 {
            // 其他活動 tick 使用 debug! 級別
            trace!(
                "Tick #{}: {} market events, {} exec events, {} orders, snapshot: {}",
                self.stats.cycle_count,
                total_events,
                exec_processed,
                result.orders_generated,
                result.snapshot_published
            );
        }

        // 定期同步延遲統計到 Prometheus（每 100 個 tick 或有活動時）
        if self.stats.cycle_count.is_multiple_of(100) || total_events > 0 || exec_processed > 0 {
            self.sync_latency_metrics_to_prometheus();
        }

        Ok(result)
    }

    // 注意：执行客户端连接和流准备现在由 ExecutionWorker 处理

    /// 發佈市場快照（增量版本）
    fn publish_market_snapshot(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        // 检查是否需要发布快照
        if !self.aggregation_engine.should_publish_snapshot() {
            return Ok(()); // 没有变化，跳过发布
        }

        let market_view = self.aggregation_engine.build_market_view();

        // 更新 Portfolio 的市場價格用於 mark-to-market（取任一場所的中間價）
        let mut market_prices =
            std::collections::HashMap::with_capacity(market_view.orderbooks.len());
        for vs in market_view.orderbooks.keys() {
            if let Some(mid_price) = market_view.get_mid_price_for_venue(vs) {
                market_prices.insert(vs.symbol.clone(), mid_price);
            }
        }
        if !market_prices.is_empty() {
            if let Some(pm) = &mut self.portfolio_manager {
                pm.update_market_prices(&market_prices);
            }
        }

        // 發佈快照
        self.market_snapshots.store(Arc::new(market_view));
        result.snapshot_published = true;
        result.snapshot_sequence += 1;
        // P3: 指標 - 快照翻轉與版本
        #[cfg(feature = "metrics")]
        {
            infra_metrics::MetricsRegistry::global().inc_snapshot_flips();
            infra_metrics::MetricsRegistry::global()
                .update_snapshot_version(self.aggregation_engine.snapshot_version);
        }

        // 标记快照已发布，清除变更标记
        self.aggregation_engine.mark_snapshot_published();

        debug!(
            "發佈增量市場快照 #{} (版本 {})",
            result.snapshot_sequence, self.aggregation_engine.snapshot_version
        );
        Ok(())
    }

    /// 發佈帳戶快照
    fn publish_account_snapshot(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        // Portfolio 內部已經自動更新快照，我們只需要獲取最新的
        if let Some(pm) = &self.portfolio_manager {
            let account_view = pm.reader().load();
            self.account_snapshots.store(account_view);
        }

        debug!("發佈帳戶快照 #{}", result.snapshot_sequence);
        Ok(())
    }

    /// 處理單個執行事件
    fn handle_execution_event(&mut self, event: &ExecutionEvent) -> Result<(), HftError> {
        debug!("處理執行事件: {:?}", event);
        // 廣播執行事件（最佳努力）
        let _ = self.broadcasters.exec_event_tx.send(event.clone());

        // 0. 先處理 OrderNew：註冊訂單元資料（供 Portfolio/OMS 使用）
        if let ExecutionEvent::OrderNew {
            order_id,
            symbol,
            side,
            quantity,
            timestamp,
            venue,
            strategy_id,
            ..
        } = event
        {
            // 註冊到 OMS 與 Portfolio（供後續 Fill 計算倉位/PnL）
            if let Some(om) = &mut self.order_manager {
                om.register_order(ports::RegisterOrderParams {
                    order_id: order_id.clone(),
                    client_order_id: None,
                    symbol: symbol.clone(),
                    side: *side,
                    qty: *quantity,
                    venue: *venue,
                    strategy_id: Some(strategy_id.clone()),
                });
            }
            // 記錄訂單所屬帳戶（若有策略對應帳戶）
            if let Some(account) = self.strategy_account_mapping.get(strategy_id) {
                self.order_account_map
                    .insert(order_id.clone(), account.clone());
            }
            if let Some(pm) = &mut self.portfolio_manager {
                pm.register_order(order_id.clone(), symbol.clone(), *side);
            }
            self.stats.orders_submitted = self.stats.orders_submitted.saturating_add(1);
            #[cfg(feature = "metrics")]
            infra_metrics::MetricsRegistry::global().inc_orders_submitted();
            self.order_submit_ts.insert(order_id.clone(), *timestamp);
            debug!(
                order_id = %order_id.0,
                sym = %symbol.as_str(),
                side = ?side,
                qty = %quantity.0,
                "已註冊新訂單到 OMS/Portfolio"
            );
        }

        // 1. 更新 OMS 狀態機
        let order_update = self
            .order_manager
            .as_mut()
            .and_then(|om| om.on_execution_event(event));
        if let Some(order_update) = order_update {
            debug!("訂單狀態更新: {:?}", order_update);

            // 檢查是否需要生成 OrderCompleted 事件（當訂單從非Filled變為Filled）
            if order_update.status == ports::OrderStatus::Filled
                && order_update.previous_status != ports::OrderStatus::Filled
            {
                // 創建 OrderCompleted 事件
                let completed_event = ExecutionEvent::OrderCompleted {
                    order_id: order_update.order_id.clone(),
                    final_price: order_update.avg_price.unwrap_or(hft_core::Price::zero()),
                    total_filled: order_update.cum_qty,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64,
                };

                info!(
                    "生成 OrderCompleted 事件: order_id={}, final_price={:?}, total_filled={}",
                    order_update.order_id.0, order_update.avg_price, order_update.cum_qty.0
                );

                // 遞歸調用處理 OrderCompleted 事件（但 OMS 會忽略它避免循環）
                self.handle_execution_event(&completed_event)?;
            }
        }

        // 2. 細分統計
        match event {
            ExecutionEvent::OrderAck {
                order_id,
                timestamp: _timestamp,
            } => {
                self.stats.orders_ack = self.stats.orders_ack.saturating_add(1);
                if let Some(&_start_ts) = self.order_submit_ts.get(order_id) {
                    #[cfg(feature = "metrics")]
                    {
                        let lat = (*_timestamp).saturating_sub(_start_ts) as f64;
                        infra_metrics::MetricsRegistry::global().record_order_ack_latency(lat);
                    }
                }
            }
            ExecutionEvent::Fill {
                order_id,
                timestamp,
                ..
            } => {
                self.stats.orders_filled = self.stats.orders_filled.saturating_add(1);
                #[cfg(feature = "metrics")]
                infra_metrics::MetricsRegistry::global().inc_orders_filled();
                if let Some(&_start_ts) = self.order_submit_ts.get(order_id) {
                    #[cfg(feature = "metrics")]
                    {
                        let lat = (*timestamp).saturating_sub(_start_ts) as f64;
                        infra_metrics::MetricsRegistry::global().record_order_fill_latency(lat);
                    }
                }

                // 記錄端到端DoD延遲指標（從市場事件到執行完成）
                if let Some(market_ts) = self.recent_market_event_timestamp {
                    let end_to_end_latency = timestamp.saturating_sub(market_ts) as f64;
                    #[cfg(feature = "metrics")]
                    infra_metrics::MetricsRegistry::global()
                        .record_end_to_end_latency(end_to_end_latency);
                    debug!(
                        "訂單 {} 端到端延遲: {:.2}μs",
                        order_id.0, end_to_end_latency
                    );
                }
            }
            ExecutionEvent::OrderReject { .. } => {
                self.stats.orders_rejected = self.stats.orders_rejected.saturating_add(1);
                #[cfg(feature = "metrics")]
                infra_metrics::MetricsRegistry::global().inc_orders_rejected();
            }
            ExecutionEvent::OrderCanceled { .. } => {
                self.stats.orders_canceled = self.stats.orders_canceled.saturating_add(1);
            }
            _ => {}
        }

        // 3. 更新 Portfolio 會計
        if let Some(pm) = &mut self.portfolio_manager {
            pm.on_execution_event(event);
        }

        // 4. 通知風控管理器
        if let Some(risk_manager) = &mut self.risk_manager {
            risk_manager.on_execution_event(event);
        }

        // 5. 讀取並打印最新 AccountView（驗證閉環）
        if let Some(pm) = &self.portfolio_manager {
            let av = pm.reader().load();
            info!(
                cash = %av.cash_balance,
                pos_count = av.positions.len(),
                unrealized = %av.unrealized_pnl,
                realized = %av.realized_pnl,
                "AccountView 已更新"
            );
        }

        Ok(())
    }

    // 注意：下单现在通过执行队列 + ExecutionWorker 异步处理

    // 原先的字符串型策略類型判斷過濾已移除；統一使用 Strategy::venue_scope 與策略映射

    /// 運行策略決策 (同步版本，用於 tick())
    fn run_strategies_sync(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        let account_view = self.get_account_view();

        // 處理所有待處理的市場事件（包括真實的 Bar 事件）
        let events_to_process = std::mem::take(&mut self.pending_market_events);

        for event in &events_to_process {
            // 創建延遲追蹤器用於此市場事件的處理
            let event_timestamp = self.extract_event_timestamp(event);
            let mut event_tracker = if let Some(ts) = event_timestamp {
                self.recent_market_event_timestamp = Some(ts);
                LatencyTracker::from_time(ts)
            } else {
                LatencyTracker::new()
            };

            // 日誌降噪：逐事件改為 debug 級別
            let (event_kind, symbol) = match event {
                ports::MarketEvent::Bar(bar) => ("bar", bar.symbol.as_str()),
                ports::MarketEvent::Trade(trade) => ("trade", trade.symbol.as_str()),
                ports::MarketEvent::Snapshot(snap) => ("snapshot", snap.symbol.as_str()),
                _ => ("other", ""),
            };

            debug!(event_kind, symbol, "處理市場事件給策略");

            // 復用策略意圖工作緩衝，避免熱路徑重分配
            let mut intents_work_buf = std::mem::take(&mut self.intents_work_buf);
            intents_work_buf.clear();
            let expected_capacity = self.strategies.len().saturating_mul(4);
            if intents_work_buf.capacity() < expected_capacity {
                intents_work_buf.reserve(expected_capacity - intents_work_buf.capacity());
            }

            // 收集所有策略的意圖，並自動填充 strategy_id
            let strategy_start = now_micros();

            // 使用策略實例 ID（而不是類型名稱）進行事件過濾
            // strategy_instance_ids 與 strategies Vec 順序對應

            for (strategy_idx, strategy) in self.strategies.iter_mut().enumerate() {
                // 跳過已禁用的策略
                if self.disabled_strategy_indices.contains(&strategy_idx) {
                    continue;
                }

                // 使用策略實例 ID（而非類型名稱）進行事件過濾
                let unknown_id = "unknown".to_string();
                let strategy_instance_id = self
                    .strategy_instance_ids
                    .get(strategy_idx)
                    .unwrap_or(&unknown_id);

                // 事件範疇過濾（使用策略顯式語義）：單場策略僅處理綁定場域事件
                let mut should_process = true;
                if matches!(strategy.venue_scope(), ports::VenueScope::Single) {
                    // 取得事件來源場域
                    let event_venue = match event {
                        ports::MarketEvent::Snapshot(snapshot) => snapshot.source_venue,
                        ports::MarketEvent::Update(update) => update.source_venue,
                        ports::MarketEvent::Trade(trade) => trade.source_venue,
                        ports::MarketEvent::Bar(bar) => bar.source_venue,
                        _ => None,
                    };
                    if let Some(&expected_venue) =
                        self.strategy_venue_mapping.get(strategy_instance_id)
                    {
                        if let Some(ev_venue) = event_venue {
                            should_process = expected_venue == ev_venue;
                        }
                    }
                }
                if !should_process {
                    let (event_kind, symbol, source) = match event {
                        ports::MarketEvent::Bar(bar) => {
                            ("bar", bar.symbol.as_str(), bar.source_venue)
                        }
                        ports::MarketEvent::Trade(trade) => {
                            ("trade", trade.symbol.as_str(), trade.source_venue)
                        }
                        ports::MarketEvent::Snapshot(snap) => {
                            ("snapshot", snap.symbol.as_str(), snap.source_venue)
                        }
                        _ => ("other", "", None),
                    };
                    debug!(
                        strategy = %strategy_instance_id,
                        event_kind,
                        symbol,
                        source_venue = ?source,
                        "策略跳過事件（venue 過濾）"
                    );
                    continue;
                }

                let mut intents = strategy.on_market_event(event, &account_view);

                // 自動填充 strategy_id（如果是空字串）：使用策略實例 ID 而非索引
                for intent in &mut intents {
                    if intent.strategy_id.is_empty() {
                        let sid = self
                            .strategy_instance_ids
                            .get(strategy_idx)
                            .cloned()
                            .unwrap_or_else(|| format!("strategy_{}", strategy_idx));
                        intent.strategy_id = sid;
                    }
                }

                intents_work_buf.extend(intents);
            }

            // 記錄策略階段完成和延遲
            event_tracker.record_stage(LatencyStage::Strategy);
            let strategy_latency = now_micros().saturating_sub(strategy_start);
            self.latency_monitor
                .record_latency(LatencyStage::Strategy, strategy_latency);

            // 記錄策略延遲到 Prometheus
            #[cfg(feature = "metrics")]
            infra_metrics::MetricsRegistry::global()
                .record_strategy_latency(strategy_latency as f64);

            // 通過風控審核（如果有配置風控管理器）
            let _risk_start = now_micros();
            let intents_total_before_risk = intents_work_buf.len();
            let mut intents_to_send = if let Some(risk_manager) = &mut self.risk_manager {
                // 使用新的 venue-specific 風控方法
                risk_manager.review_with_venue_specs(
                    intents_work_buf,
                    &account_view,
                    &self.venue_specs,
                )
            } else {
                intents_work_buf // 沒有風控則全部通過（移動所有權）
            };

            let risk_latency = now_micros().saturating_sub(_risk_start);
            event_tracker.record_stage(LatencyStage::Risk);
            self.latency_monitor
                .record_latency(LatencyStage::Risk, risk_latency);
            #[cfg(feature = "metrics")]
            infra_metrics::MetricsRegistry::global().record_risk_latency(risk_latency as f64);

            let orders_count = intents_to_send.len() as u32;
            result.orders_generated += orders_count;

            // 日誌降噪：只有在有訂單時才記錄
            if orders_count > 0 {
                debug!(
                    "策略生成 {} 個訂單意圖，風控通過 {} 個",
                    intents_total_before_risk,
                    intents_to_send.len()
                );
            }
            if !intents_to_send.is_empty() {
                let market_view = self.get_market_view();
                for intent in &mut intents_to_send {
                    if matches!(intent.order_type, OrderType::Market) && intent.price.is_none() {
                        // 優先使用目標場館的最佳價，否則回退為任一場所
                        let preferred = if let Some(venue) = intent.target_venue {
                            let key = hft_core::VenueSymbol::new(venue, intent.symbol.clone());
                            match intent.side {
                                Side::Buy => {
                                    market_view.get_best_ask_for_venue(&key).map(|(p, _)| p)
                                }
                                Side::Sell => {
                                    market_view.get_best_bid_for_venue(&key).map(|(p, _)| p)
                                }
                            }
                        } else {
                            None
                        };

                        let best_any = match intent.side {
                            Side::Buy => {
                                market_view.get_best_ask_any(&intent.symbol).map(|(p, _)| p)
                            }
                            Side::Sell => {
                                market_view.get_best_bid_any(&intent.symbol).map(|(p, _)| p)
                            }
                        };

                        if let Some(px) = preferred
                            .or(best_any)
                            .or_else(|| market_view.get_mid_price_any(&intent.symbol))
                        {
                            intent.price = Some(px);
                        }
                    }
                }
            }

            // 發送通過風控的意圖到执行队列
            let execution_start = now_micros();
            if let Some(queues) = &mut self.execution_queues {
                let mut dropped = 0usize;
                for intent in intents_to_send.drain(..) {
                    if queues.send_intent(intent).is_err() {
                        dropped += 1;
                    }
                }
                if dropped > 0 {
                    warn!("{}个意图因队列满载被丢弃", dropped);
                    // P3: 更新引擎統計與指標
                    self.stats.intents_dropped =
                        self.stats.intents_dropped.saturating_add(dropped as u64);
                    #[cfg(feature = "metrics")]
                    infra_metrics::MetricsRegistry::global().add_intents_dropped(dropped as u64);
                }
            } else {
                intents_to_send.clear();
            }

            // 將複用緩衝歸還給引擎，保留已擴容容量
            self.intents_work_buf = intents_to_send;

            // 記錄執行階段延遲到統一監控器
            let execution_latency = now_micros().saturating_sub(execution_start);
            event_tracker.record_stage(LatencyStage::Execution);
            self.latency_monitor
                .record_latency(LatencyStage::Execution, execution_latency);

            // 如果有起始時間戳，計算端到端延遲
            if let Some(origin_ts) = event_timestamp {
                let end_to_end_latency = now_micros().saturating_sub(origin_ts);
                event_tracker.record_stage(LatencyStage::EndToEnd);
                self.latency_monitor
                    .record_latency(LatencyStage::EndToEnd, end_to_end_latency);
            }

            // 記錄執行延遲到 Prometheus
            #[cfg(feature = "metrics")]
            infra_metrics::MetricsRegistry::global()
                .record_execution_latency(execution_latency as f64);
        }

        Ok(())
    }

    /// 主循環
    pub async fn run(&mut self) -> Result<(), HftError> {
        info!("引擎開始運行");
        self.stats.is_running = true;

        // 採用事件驅動 + 輕量退避：
        // - 正常情況完全事件驅動（Notify 喚醒）
        // - 若上游未傳遞 Notify（某些適配器路徑），小幅超時作為保險
        let mut backoff_us: u64 = 50; // 初始 50us
        let max_backoff_us: u64 = 10_000; // 最大 10ms（極低負載時）

        while self.stats.is_running {
            // 等待事件或超時計時（保證 liveness）
            tokio::select! {
                _ = self.wakeup_notify.notified() => {
                    backoff_us = 50; // 收到事件，重置退避
                }
                _ = tokio::time::sleep(std::time::Duration::from_micros(backoff_us)) => {
                    // 無通知，維持/增加退避
                    backoff_us = (backoff_us.saturating_mul(2)).min(max_backoff_us);
                }
            }

            // 被喚醒/超時後，緊湊處理直到隊列清空
            loop {
                match self.tick() {
                    Ok(tick_result) => {
                        // 無活動則退出內層循環，回到等待狀態
                        if tick_result.events_total == 0
                            && tick_result.execution_events_processed == 0
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("引擎循環錯誤: {}", e);
                        // 輕微等待避免刷屏後返回等待狀態
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        break;
                    }
                }
            }
        }

        info!("引擎停止運行");
        Ok(())
    }

    /// 停止引擎
    pub fn stop(&mut self) {
        info!("正在停止引擎");
        self.stats.is_running = false;
        // 確保喚醒 run() 內的等待者，讓其能夠立即退出
        self.wakeup_notify.notify_waiters();
    }

    // === Sentinel 控制方法 ===

    /// 獲取當前交易模式
    pub fn trading_mode(&self) -> TradingMode {
        self.stats.trading_mode
    }

    /// 設置交易模式（由 Sentinel 調用）
    pub fn set_trading_mode(&mut self, mode: TradingMode) {
        if self.stats.trading_mode != mode {
            info!("交易模式變更: {:?} -> {:?}", self.stats.trading_mode, mode);
            self.stats.trading_mode = mode;
        }
    }

    /// 暫停交易（不生成新訂單）
    pub fn pause_trading(&mut self) {
        warn!("Sentinel: 暫停交易");
        self.stats.trading_mode = TradingMode::Paused;
    }

    /// 恢復正常交易
    pub fn resume_trading(&mut self) {
        info!("Sentinel: 恢復正常交易");
        self.stats.trading_mode = TradingMode::Normal;
    }

    /// 進入降頻模式
    pub fn enter_degrade_mode(&mut self) {
        warn!("Sentinel: 進入降頻模式");
        self.stats.trading_mode = TradingMode::Degraded;
    }

    /// 觸發緊急平倉
    pub fn emergency_exit(&mut self) -> Vec<(hft_core::OrderId, hft_core::Symbol)> {
        error!("Sentinel: 緊急平倉觸發！");
        self.stats.trading_mode = TradingMode::Emergency;

        // 收集所有策略的未結訂單用於平倉
        let mut all_open_orders = Vec::new();
        for strategy_id in &self.strategy_instance_ids {
            let orders = self.open_order_pairs_by_strategy(strategy_id);
            all_open_orders.extend(orders);
        }

        info!("緊急平倉: 發現 {} 個待取消訂單", all_open_orders.len());
        all_open_orders
    }

    /// 獲取系統統計用於 Sentinel（延遲 + PnL）
    pub fn get_sentinel_stats(&self) -> SentinelStats {
        // 從延遲監控器獲取真實延遲
        let latency_stats = self.latency_monitor.get_stage_stats(hft_core::LatencyStage::EndToEnd);
        let (latency_p99_us, latency_p50_us) = latency_stats
            .map(|s| (s.p99_micros, s.p50_micros))
            .unwrap_or((0, 0));

        // 從 Portfolio 獲取 PnL 和回撤（如果有）
        let (pnl, unrealized_pnl, drawdown_pct, max_drawdown_pct, high_water_mark) =
            if let Some(pm) = &self.portfolio_manager {
                let av = pm.reader().load();
                let total_pnl = av.total_pnl();
                (
                    total_pnl.to_string().parse::<f64>().unwrap_or(0.0),
                    av.unrealized_pnl.to_string().parse::<f64>().unwrap_or(0.0),
                    av.drawdown_pct,
                    av.max_drawdown_pct,
                    av.high_water_mark.to_string().parse::<f64>().unwrap_or(0.0),
                )
            } else {
                (0.0, 0.0, 0.0, 0.0, 0.0)
            };

        SentinelStats {
            latency_p99_us,
            latency_p50_us,
            pnl,
            unrealized_pnl,
            drawdown_pct,
            max_drawdown_pct,
            high_water_mark,
        }
    }

    /// 從市場事件中提取時間戳
    fn extract_event_timestamp(&self, event: &ports::MarketEvent) -> Option<u64> {
        match event {
            ports::MarketEvent::Bar(bar) => Some(bar.close_time),
            ports::MarketEvent::Trade(trade) => Some(trade.timestamp),
            ports::MarketEvent::Snapshot(snapshot) => Some(snapshot.timestamp),
            ports::MarketEvent::Update(update) => Some(update.timestamp),
            _ => None,
        }
    }

    /// 獲取引擎統計
    pub fn get_statistics(&self) -> EngineStatistics {
        // 從延遲監控器收集各階段延遲統計
        let latency = self.collect_latency_stats();

        // 計算運行時間
        let uptime_us = now_micros().saturating_sub(self.stats.start_time_us);
        let uptime_seconds = uptime_us / 1_000_000;

        EngineStatistics {
            cycle_count: self.stats.cycle_count,
            consumers_count: self.event_consumers.len(),
            strategies_count: self.strategies.len(),
            execution_clients_count: self.execution_clients.len(),
            execution_events_processed: self.stats.execution_events_processed,
            is_running: self.stats.is_running,
            orders_submitted: self.stats.orders_submitted,
            orders_ack: self.stats.orders_ack,
            orders_filled: self.stats.orders_filled,
            orders_rejected: self.stats.orders_rejected,
            orders_canceled: self.stats.orders_canceled,
            latency,
            uptime_seconds,
        }
    }

    /// 收集延遲統計數據
    fn collect_latency_stats(&self) -> EngineLatencyStats {
        use hft_core::LatencyStage;

        let mut stats = EngineLatencyStats::default();

        // 端到端延遲 (最重要的指標)
        if let Some(e2e) = self.latency_monitor.get_stage_stats(LatencyStage::EndToEnd) {
            stats.end_to_end_p50_us = e2e.p50_micros;
            stats.end_to_end_p95_us = e2e.p95_micros;
            stats.end_to_end_p99_us = e2e.p99_micros;
            stats.sample_count = e2e.count;
        }

        // 各階段 p99 延遲
        if let Some(s) = self.latency_monitor.get_stage_stats(LatencyStage::Ingestion) {
            stats.ingestion_p99_us = s.p99_micros;
        }
        if let Some(s) = self.latency_monitor.get_stage_stats(LatencyStage::Aggregation) {
            stats.aggregation_p99_us = s.p99_micros;
        }
        if let Some(s) = self.latency_monitor.get_stage_stats(LatencyStage::Strategy) {
            stats.strategy_p99_us = s.p99_micros;
        }
        if let Some(s) = self.latency_monitor.get_stage_stats(LatencyStage::Risk) {
            stats.risk_p99_us = s.p99_micros;
        }
        if let Some(s) = self.latency_monitor.get_stage_stats(LatencyStage::Execution) {
            stats.execution_p99_us = s.p99_micros;
        }

        stats
    }

    /// 根據訂單查詢帳戶（Phase 1）
    pub fn get_account_for_order(
        &self,
        order_id: &hft_core::OrderId,
    ) -> Option<hft_core::AccountId> {
        self.order_account_map.get(order_id).cloned()
    }

    /// 獲取系統背壓狀態（用於運行時監控）
    pub fn get_backpressure_status(&self) -> Vec<BackpressureStatus> {
        let mut statuses = Vec::new();

        for (idx, consumer) in self.event_consumers.iter().enumerate() {
            let utilization = consumer.utilization();
            let recommended_action = if utilization >= 0.9 {
                format!(
                    "Consumer {} saturated - consider sharding or increasing queue",
                    idx
                )
            } else if utilization >= 0.8 {
                format!("Consumer {} above 80% utilization - monitor closely", idx)
            } else {
                format!("Consumer {} healthy", idx)
            };

            statuses.push(BackpressureStatus {
                utilization,
                is_under_pressure: utilization >= 0.8,
                events_dropped_total: self.stats.market_events_dropped,
                recommended_action,
                queue_capacity: consumer.queue_capacity(),
            });
        }

        if let Some(exec_queues) = &self.execution_queues {
            let queue_stats = *exec_queues.stats();

            let intent_util = exec_queues.intent_queue_utilization();
            statuses.push(BackpressureStatus {
                utilization: intent_util,
                is_under_pressure: intent_util >= 0.8,
                events_dropped_total: queue_stats.intent_queue_full_count
                    + self.stats.intents_dropped,
                recommended_action: if intent_util >= 0.8 {
                    "Execution intent queue saturated - throttle strategy output or enlarge queue"
                        .to_string()
                } else {
                    "Execution intent queue healthy".to_string()
                },
                queue_capacity: exec_queues.intent_queue_capacity(),
            });

            let event_util = exec_queues.event_queue_utilization();
            statuses.push(BackpressureStatus {
                utilization: event_util,
                is_under_pressure: event_util >= 0.8,
                events_dropped_total: queue_stats.event_queue_full_count,
                recommended_action: if event_util >= 0.8 {
                    "Execution event queue saturated - ensure workers drain events promptly"
                        .to_string()
                } else {
                    "Execution event queue healthy".to_string()
                },
                queue_capacity: exec_queues.event_queue_capacity(),
            });
        }

        if statuses.is_empty() {
            statuses.push(BackpressureStatus {
                utilization: 0.0,
                is_under_pressure: false,
                events_dropped_total: self.stats.market_events_dropped + self.stats.intents_dropped,
                recommended_action: "No active event consumers or execution queues".to_string(),
                queue_capacity: self.config.ingestion.queue_capacity,
            });
        }

        statuses
    }
}

/// 單次 tick 的結果
#[derive(Debug, Default)]
pub struct EngineTickResult {
    pub events_processed: u32,
    pub events_total: u32,
    pub execution_events_processed: u32,
    pub snapshot_published: bool,
    pub snapshot_sequence: u64,
    pub orders_generated: u32,
}

/// Sentinel 監控所需的統計數據
#[derive(Debug, Clone, Default)]
pub struct SentinelStats {
    /// 端到端延遲 p99 (微秒)
    pub latency_p99_us: u64,
    /// 端到端延遲 p50 (微秒)
    pub latency_p50_us: u64,
    /// 總 PnL (已實現 + 未實現)
    pub pnl: f64,
    /// 未實現 PnL
    pub unrealized_pnl: f64,
    /// 當前回撤百分比
    pub drawdown_pct: f64,
    /// 歷史最大回撤百分比
    pub max_drawdown_pct: f64,
    /// 高水位標記
    pub high_water_mark: f64,
}

/// 引擎延遲統計信息
#[derive(Debug, Clone, Default)]
pub struct EngineLatencyStats {
    /// 端到端延遲 p50 (微秒)
    pub end_to_end_p50_us: u64,
    /// 端到端延遲 p95 (微秒)
    pub end_to_end_p95_us: u64,
    /// 端到端延遲 p99 (微秒)
    pub end_to_end_p99_us: u64,
    /// 數據攝取延遲 p99 (微秒)
    pub ingestion_p99_us: u64,
    /// 聚合階段延遲 p99 (微秒)
    pub aggregation_p99_us: u64,
    /// 策略階段延遲 p99 (微秒)
    pub strategy_p99_us: u64,
    /// 風控階段延遲 p99 (微秒)
    pub risk_p99_us: u64,
    /// 執行階段延遲 p99 (微秒)
    pub execution_p99_us: u64,
    /// 樣本數量
    pub sample_count: u64,
}

/// 引擎統計信息
#[derive(Debug)]
pub struct EngineStatistics {
    pub cycle_count: u64,
    pub consumers_count: usize,
    pub strategies_count: usize,
    pub execution_clients_count: usize,
    pub execution_events_processed: u64,
    pub is_running: bool,
    pub orders_submitted: u64,
    pub orders_ack: u64,
    pub orders_filled: u64,
    pub orders_rejected: u64,
    pub orders_canceled: u64,
    /// 延遲統計
    pub latency: EngineLatencyStats,
    /// 引擎運行時間（秒）
    pub uptime_seconds: u64,
}

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use super::*;
    use hft_core::{Price, Quantity, Symbol, VenueId};
    use ports::{AggregatedBar, MarketEvent};

    struct SingleVenueStub {
        id: String,
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl SingleVenueStub {
        fn new(id: &str, calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
            Self {
                id: id.to_string(),
                calls,
            }
        }
    }

    impl ports::Strategy for SingleVenueStub {
        fn on_market_event(
            &mut self,
            _event: &ports::MarketEvent,
            _account: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            self.calls.lock().unwrap().push(self.id.clone());
            Vec::new()
        }
        fn on_execution_event(
            &mut self,
            _event: &ports::ExecutionEvent,
            _account: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            Vec::new()
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn venue_scope(&self) -> ports::VenueScope {
            ports::VenueScope::Single
        }
    }

    struct CrossVenueStub {
        id: String,
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl CrossVenueStub {
        fn new(id: &str, calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
            Self {
                id: id.to_string(),
                calls,
            }
        }
    }

    impl ports::Strategy for CrossVenueStub {
        fn on_market_event(
            &mut self,
            _event: &ports::MarketEvent,
            _account: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            self.calls.lock().unwrap().push(self.id.clone());
            Vec::new()
        }
        fn on_execution_event(
            &mut self,
            _event: &ports::ExecutionEvent,
            _account: &ports::AccountView,
        ) -> Vec<ports::OrderIntent> {
            Vec::new()
        }
        fn name(&self) -> &str {
            &self.id
        }
        fn venue_scope(&self) -> ports::VenueScope {
            ports::VenueScope::Cross
        }
    }

    #[test]
    fn test_engine_creation() {
        let config = EngineConfig::default();
        let engine = Engine::new(config);

        assert_eq!(engine.strategies.len(), 0);
        assert_eq!(engine.event_consumers.len(), 0);
    }

    #[test]
    fn test_strategy_event_filtering_trait_based() {
        // 構造引擎
        let config = EngineConfig::default();
        let mut engine = Engine::new(config);

        // 建立共享記錄容器
        let calls: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();

        // 註冊策略：單場與跨場
        engine.register_strategy(SingleVenueStub::new("single1", calls.clone()));
        engine.register_strategy(CrossVenueStub::new("cross1", calls.clone()));

        // 配置策略到場館映射：單場策略綁定 BITGET
        let mut mapping = HashMap::new();
        mapping.insert("single1".to_string(), VenueId::BITGET);
        engine.set_strategy_venue_mapping(mapping);

        // 事件：Bar 來自 BINANCE（應該只命中 cross1）
        let bar_event = MarketEvent::Bar(AggregatedBar {
            symbol: Symbol::new("BTCUSDT"),
            interval_ms: 60000,
            open_time: 1000000,
            close_time: 1060000,
            open: Price::from_f64(50000.0).unwrap(),
            high: Price::from_f64(51000.0).unwrap(),
            low: Price::from_f64(49000.0).unwrap(),
            close: Price::from_f64(50500.0).unwrap(),
            volume: Quantity::from_f64(10.0).unwrap(),
            trade_count: 100,
            source_venue: Some(VenueId::BINANCE),
        });

        let account = AccountView::default();
        engine.process_market_event_for_strategies(&bar_event, &account);

        let got = calls.lock().unwrap().clone();
        assert!(got.contains(&"cross1".to_string()));
        assert!(!got.contains(&"single1".to_string()));
    }
}
