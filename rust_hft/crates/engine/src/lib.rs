//! Single-writer engine loop（骨架）
//! - Ingest bounded rings from I/O
//! - Aggregate L2 TopN and Bars
//! - Publish snapshots via snapshot crate
//! - Route: Strategy -> Risk -> Execution

pub mod dataflow;
pub mod aggregation;
pub mod execution_queues;
pub mod execution_worker;
pub mod adapter_bridge;
pub mod latency_monitor;

use ports::{ExecutionClient, Strategy, ExecutionEvent, AccountView, BoxStream};
use dataflow::{EventConsumer, IngestionConfig};
use aggregation::{AggregationEngine, MarketView};
use execution_queues::EngineQueues;
use snapshot::SnapshotContainer;
use hft_core::{HftError, Symbol};
use oms_core::OmsCore;
use portfolio_core::Portfolio;
use tracing::{info, warn, error, debug};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Notify;

// 重新導出關鍵類型
pub use adapter_bridge::{AdapterBridge, AdapterBridgeConfig};
pub use dataflow::{FlipPolicy, BackpressurePolicy};
pub use execution_queues::{create_execution_queues, WorkerQueues, ExecutionQueueConfig};
pub use execution_worker::{ExecutionWorker, ExecutionWorkerConfig, spawn_execution_worker};

/// 引擎運行時配置
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub ingestion: IngestionConfig,
    pub max_events_per_cycle: u32,
    pub aggregation_symbols: Vec<Symbol>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            ingestion: IngestionConfig::default(),
            max_events_per_cycle: 100,
            aggregation_symbols: vec![],
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
    /// 註冊的執行客戶端
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    /// 執行回報流（與 execution_clients 對應）
    #[allow(dead_code)]
    execution_streams: Vec<BoxStream<ExecutionEvent>>,
    /// OMS 核心 - 訂單生命週期管理
    oms_core: OmsCore,
    /// Portfolio 核心 - 會計與資金管理
    portfolio: Portfolio,
    /// 执行队列系统 (SPSC)
    execution_queues: Option<EngineQueues>,
    /// 運行統計
    cycle_count: u64,
    is_running: bool,
    /// 已處理的執行事件計數（用於調試）
    execution_events_processed: u64,
    /// 暫存的市場事件（從聚合引擎產生）
    pending_market_events: Vec<ports::MarketEvent>,
    /// 事件驱动唤醒通知器
    wakeup_notify: Arc<Notify>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        // 創建初始空的市場視圖
        let initial_market_view = MarketView {
            orderbooks: std::collections::HashMap::new(),
            arbitrage_opportunities: Vec::new(),
            timestamp: 0,
            version: 0,
        };
        
        // 創建初始空的帳戶視圖
        let initial_account_view = AccountView::default();
        
        Self {
            config,
            event_consumers: Vec::new(),
            aggregation_engine: AggregationEngine::new(),
            market_snapshots: SnapshotContainer::new(initial_market_view),
            account_snapshots: SnapshotContainer::new(initial_account_view),
            strategies: Vec::new(),
            risk_manager: None,
            execution_clients: Vec::new(),
            execution_streams: Vec::new(),
            oms_core: OmsCore::new(),
            portfolio: Portfolio::new(),
            execution_queues: None,
            cycle_count: 0,
            is_running: true,  // 默認為運行狀態，讓 tick() 可以正常運作
            execution_events_processed: 0,
            pending_market_events: Vec::new(),
            wakeup_notify: Arc::new(Notify::new()),
        }
    }

    /// 设置执行队列系统（Runtime 设置）
    pub fn set_execution_queues(&mut self, queues: EngineQueues) {
        self.execution_queues = Some(queues);
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
        
        let (ingester, consumer) = dataflow::EventIngester::new(config);
        
        // 註冊消費者到引擎
        self.register_event_consumer(consumer);
        
        // 返回攝取器供零拷貝適配器使用
        Arc::new(std::sync::Mutex::new(ingester))
    }
    
    /// 註冊策略
    pub fn register_strategy<S: Strategy + 'static>(&mut self, s: S) {
        self.strategies.push(Box::new(s));
    }
    
    pub fn register_strategy_boxed(&mut self, s: Box<dyn Strategy>) {
        self.strategies.push(s);
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
                    warn!("執行隊列滿載，訂單意圖被拒絕: {} {}", 
                          rejected_intent.symbol.0, rejected_intent.quantity.0);
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
        self.cycle_count += 1;
        let mut result = EngineTickResult::default();
        
        // 只在前 100 個循環和有事件時記錄
        if self.cycle_count <= 100 || self.cycle_count % 1000 == 0 {
            debug!("引擎 tick #{}", self.cycle_count);
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
                
                // 日誌降噪：逐事件日誌改為 debug 級別
                debug!("引擎 tick #{} 處理事件 #{} 從消費者 {}: {:?}", 
                      self.cycle_count, total_events, idx,
                      match &event {
                          ports::MarketEvent::Bar(bar) => format!("Bar({})", bar.symbol.0),
                          ports::MarketEvent::Trade(trade) => format!("Trade({})", trade.symbol.0),
                          ports::MarketEvent::Snapshot(snap) => format!("Snapshot({})", snap.symbol.0),
                          _ => "Other".to_string(),
                      });
                
                // 處理事件 -> 聚合引擎
                match self.aggregation_engine.handle_event(event) {
                    Ok(events) => {
                        // 保存生成的事件（包括 Bar 事件）
                        let event_count = events.len();
                        for ev in events {
                            self.pending_market_events.push(ev);
                        }
                        if event_count > 0 {
                            debug!("聚合引擎生成 {} 個事件", event_count);
                        }
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
        if let Some(queues) = &mut self.execution_queues {
            let execution_events = queues.receive_events();
            for ev in &execution_events {
                self.handle_execution_event(ev)?;
                exec_processed += 1;
                self.execution_events_processed += 1;
            }
        }
        result.execution_events_processed = exec_processed;
        
        // 階段 3: 檢查是否需要發佈快照
        if should_flip || self.aggregation_engine.should_publish_snapshot() || result.execution_events_processed > 0 {
            self.publish_market_snapshot(&mut result)?;
            self.publish_account_snapshot(&mut result)?;
        }
        
        // 階段 4: 策略決策 (基於最新快照)
        if result.snapshot_published {
            // Note: run_strategies simplified to sync for tick() compatibility
            self.run_strategies_sync(&mut result)?;
        }
        
        // 批量統計日誌（只有在有活動時才記錄）
        if total_events > 0 || exec_processed > 0 || result.orders_generated > 0 {
            info!("Tick #{}: {} market events, {} exec events, {} orders, snapshot: {}", 
                  self.cycle_count, total_events, exec_processed, 
                  result.orders_generated, result.snapshot_published);
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
        
        // 更新 Portfolio 的市場價格用於 mark-to-market
        let mut market_prices = std::collections::HashMap::new();
        for (symbol, _) in &market_view.orderbooks {
            if let Some(mid_price) = market_view.get_mid_price(symbol) {
                market_prices.insert(symbol.clone(), mid_price);
            }
        }
        if !market_prices.is_empty() {
            self.portfolio.update_market_prices(&market_prices);
        }
        
        // 發佈快照
        self.market_snapshots.store(Arc::new(market_view));
        result.snapshot_published = true;
        result.snapshot_sequence += 1;
        
        // 标记快照已发布，清除变更标记
        self.aggregation_engine.mark_snapshot_published();
        
        debug!("發佈增量市場快照 #{} (版本 {})", 
               result.snapshot_sequence, 
               self.aggregation_engine.snapshot_version);
        Ok(())
    }
    
    /// 發佈帳戶快照  
    fn publish_account_snapshot(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        // Portfolio 內部已經自動更新快照，我們只需要獲取最新的
        let account_view = self.portfolio.reader().load();
        self.account_snapshots.store(account_view);
        
        debug!("發佈帳戶快照 #{}", result.snapshot_sequence);
        Ok(())
    }
    
    
    /// 處理單個執行事件
    fn handle_execution_event(&mut self, event: &ExecutionEvent) -> Result<(), HftError> {
        debug!("處理執行事件: {:?}", event);
        
        // 1. 更新 OMS 狀態機
        if let Some(order_update) = self.oms_core.on_execution_event(event) {
            debug!("訂單狀態更新: {:?}", order_update);
            
            // 檢查是否需要生成 OrderCompleted 事件（當訂單從非Filled變為Filled）
            if order_update.status == oms_core::OrderStatus::Filled 
               && order_update.previous_status != oms_core::OrderStatus::Filled {
                
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
                
                info!("生成 OrderCompleted 事件: order_id={}, final_price={:?}, total_filled={}", 
                      order_update.order_id.0, 
                      order_update.avg_price, 
                      order_update.cum_qty.0);
                
                // 遞歸調用處理 OrderCompleted 事件（但 OMS 會忽略它避免循環）
                self.handle_execution_event(&completed_event)?;
            }
        }
        
        // 2. 更新 Portfolio 會計
        self.portfolio.on_execution_event(event);
        
        // 3. 通知風控管理器
        if let Some(risk_manager) = &mut self.risk_manager {
            risk_manager.on_execution_event(event);
        }
        
        // 4. 讀取並打印最新 AccountView（驗證閉環）
        let av = self.portfolio.reader().load();
        info!(
            cash = av.cash_balance,
            pos_count = av.positions.len(),
            unrealized = av.unrealized_pnl,
            realized = av.realized_pnl,
            "AccountView 已更新"
        );
        
        Ok(())
    }
    
    // 注意：下单现在通过执行队列 + ExecutionWorker 异步处理
    
    /// 運行策略決策 (同步版本，用於 tick())
    fn run_strategies_sync(&mut self, result: &mut EngineTickResult) -> Result<(), HftError> {
        let account_view = self.get_account_view();
        
        // 處理所有待處理的市場事件（包括真實的 Bar 事件）
        let events_to_process = std::mem::take(&mut self.pending_market_events);
        
        for event in &events_to_process {
            // 日誌降噪：逐事件改為 debug 級別
            debug!("處理市場事件給策略: {:?}", 
                   match event {
                       ports::MarketEvent::Bar(bar) => format!("Bar({})", bar.symbol.0),
                       ports::MarketEvent::Trade(trade) => format!("Trade({})", trade.symbol.0),
                       ports::MarketEvent::Snapshot(snap) => format!("Snapshot({})", snap.symbol.0),
                       _ => "Other".to_string(),
                   });
            
            let mut all_intents = Vec::new();
            
            // 收集所有策略的意圖，並自動填充 strategy_id
            for (strategy_idx, strategy) in self.strategies.iter_mut().enumerate() {
                let mut intents = strategy.on_market_event(event, &account_view);
                
                // 自動填充 strategy_id（如果是空字串）
                for intent in &mut intents {
                    if intent.strategy_id.is_empty() {
                        intent.strategy_id = format!("strategy_{}", strategy_idx);
                    }
                }
                
                all_intents.extend(intents);
            }
            
            // 通過風控審核（如果有配置風控管理器）
            let approved_intents = if let Some(risk_manager) = &mut self.risk_manager {
                // 使用 ports::VenueSpec 的默認值
                let venue_spec = ports::VenueSpec::default();
                risk_manager.review(all_intents.clone(), &account_view, &venue_spec)
            } else {
                all_intents.clone() // 沒有風控則全部通過
            };
            
            let orders_count = approved_intents.len() as u32;
            result.orders_generated += orders_count;
            
            // 日誌降噪：只有在有訂單時才記錄
            if orders_count > 0 {
                debug!("策略生成 {} 個訂單意圖，風控通過 {} 個", 
                       all_intents.len(), approved_intents.len());
            }
            
            // 發送通過風控的意圖到执行队列
            if let Some(queues) = &mut self.execution_queues {
                let failed_intents = queues.send_intents_batch(approved_intents);
                if !failed_intents.is_empty() {
                    warn!("{}个意图因队列满载被丢弃", failed_intents.len());
                }
            }
        }
        
        Ok(())
    }
    
    /// 主循環
    pub async fn run(&mut self) -> Result<(), HftError> {
        info!("引擎開始運行");
        self.is_running = true;
        
        while self.is_running {
            match self.tick() {
                Ok(tick_result) => {
                    if tick_result.events_total > 0 {
                        debug!("Tick #{}: {} 事件, 快照: {}", 
                               self.cycle_count, 
                               tick_result.events_total,
                               tick_result.snapshot_published);
                    }
                    
                    // 檢查是否需要休眠 (如果沒有事件處理)
                    if tick_result.events_total == 0 {
                        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                    }
                }
                Err(e) => {
                    error!("引擎循環錯誤: {}", e);
                    // 可以選擇繼續或停止
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        }
        
        info!("引擎停止運行");
        Ok(())
    }
    
    /// 停止引擎
    pub fn stop(&mut self) {
        info!("正在停止引擎");
        self.is_running = false;
    }
    
    /// 獲取引擎統計
    pub fn get_statistics(&self) -> EngineStatistics {
        EngineStatistics {
            cycle_count: self.cycle_count,
            consumers_count: self.event_consumers.len(),
            strategies_count: self.strategies.len(),
            execution_clients_count: self.execution_clients.len(),
            execution_events_processed: self.execution_events_processed,
            is_running: self.is_running,
        }
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

/// 引擎統計信息
#[derive(Debug)]
pub struct EngineStatistics {
    pub cycle_count: u64,
    pub consumers_count: usize,
    pub strategies_count: usize,
    pub execution_clients_count: usize,
    pub execution_events_processed: u64,
    pub is_running: bool,
}
