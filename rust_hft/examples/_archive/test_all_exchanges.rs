/*!
 * 多交易所多商品並發測試 - 全面的市場數據接收測試
 *
 * 功能：
 * 1. 同時測試多個交易所 (Bitget, Binance, Bybit)
 * 2. 並發訂閱多個交易對
 * 3. 實時統計各交易所各商品的數據接收率
 * 4. 提供綜合性能分析報告
 */

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tracing::{error, info, warn};

use rust_hft::exchanges::exchange_trait::Exchange;
use rust_hft::exchanges::message_types::MarketEvent;
use rust_hft::exchanges::{BinanceExchange, BitgetExchange, BybitExchange, ExchangeManager};

/// 單個交易對的測試結果
#[derive(Debug, Clone)]
pub struct SymbolTestResult {
    pub symbol: String,
    pub exchange: String,
    pub connection_success: bool,
    pub subscription_success: bool,
    pub total_events: u64,
    pub orderbook_events: u64,
    pub trade_events: u64,
    pub ticker_events: u64,
    pub events_per_second: f64,
    pub test_duration_secs: f64,
}

/// 多交易所多商品測試框架
pub struct MultiExchangeMultiSymbolTest {
    test_symbols: Vec<String>,
    test_duration_secs: u64,
    exchange_manager: Arc<ExchangeManager>,
}

impl MultiExchangeMultiSymbolTest {
    /// 創建新的測試框架 - 修復版：避免共享 ExchangeManager
    pub async fn new(test_symbols: Vec<String>, test_duration_secs: u64) -> Result<Self> {
        // 不再預先創建交易所，避免並發衝突
        info!("🏗️ 初始化測試框架 (延遲創建交易所實例)");

        Ok(Self {
            test_symbols,
            test_duration_secs,
            exchange_manager: Arc::new(ExchangeManager::new()), // 空的管理器
        })
    }

    /// 運行多交易所多商品並發測試 - 使用真正的並發架構
    pub async fn run_comprehensive_test(&self) -> Result<Vec<SymbolTestResult>> {
        info!("🚀 開始多交易所多商品並發測試");
        info!("   測試商品: {:?}", self.test_symbols);
        info!("   測試時長: {} 秒", self.test_duration_secs);
        info!("   測試模式: 真正並發 - 每交易所每商品獨立連接 (基於 concurrent_performance_test)");

        let exchanges = ["bitget", "binance", "bybit"];
        let mut test_handles = Vec::new();

        // 真正並發：每個交易所+商品組合都獨立測試（獨立交易所實例）
        for exchange_name in &exchanges {
            for symbol in &self.test_symbols {
                let test_symbol = symbol.clone();
                let test_duration_secs = self.test_duration_secs;
                let exchange_name = exchange_name.to_string();

                let handle = tokio::spawn(async move {
                    Self::test_single_exchange_symbol_independent_static(
                        &exchange_name,
                        &test_symbol,
                        test_duration_secs,
                    )
                    .await
                });
                test_handles.push(handle);
            }
        }

        // 等待所有測試完成
        let results = futures::future::join_all(test_handles).await;
        let flattened_results: Vec<SymbolTestResult> =
            results.into_iter().filter_map(|r| r.ok()).collect();

        // 打印綜合分析報告
        self.print_comprehensive_analysis(&flattened_results).await;

        Ok(flattened_results)
    }

    /// 測試單個交易所的多個商品 (靜態方法) - 修復版
    async fn test_single_exchange_multi_symbols_static(
        exchange_manager: Arc<ExchangeManager>,
        exchange_name: &str,
        symbols: &[String],
        test_duration_secs: u64,
    ) -> Vec<SymbolTestResult> {
        info!("🔗 [{}] 開始測試 {} 個商品", exchange_name, symbols.len());

        let mut results = Vec::new();

        // 為每個商品初始化結果
        for symbol in symbols {
            results.push(SymbolTestResult {
                symbol: symbol.clone(),
                exchange: exchange_name.to_string(),
                connection_success: false,
                subscription_success: false,
                total_events: 0,
                orderbook_events: 0,
                trade_events: 0,
                ticker_events: 0,
                events_per_second: 0.0,
                test_duration_secs: 0.0,
            });
        }

        // 獲取交易所實例
        let exchange_ref = match exchange_manager.get_exchange(exchange_name).await {
            Some(exchange) => exchange,
            None => {
                error!("❌ [{}] 交易所實例未找到", exchange_name);
                return results;
            }
        };

        // 階段1: 單次連接
        {
            let mut exchange = exchange_ref.write().await;
            match timeout(Duration::from_secs(15), exchange.connect_public()).await {
                Ok(Ok(())) => {
                    for result in &mut results {
                        result.connection_success = true;
                    }
                    info!("✅ [{}] 單次連接成功", exchange_name);
                }
                Ok(Err(e)) => {
                    error!("❌ [{}] 連接失敗: {}", exchange_name, e);
                    return results;
                }
                Err(_) => {
                    error!("❌ [{}] 連接超時", exchange_name);
                    return results;
                }
            }
        }

        // 階段2: 批量訂閱所有商品
        {
            let mut exchange = exchange_ref.write().await;
            let mut successful_subscriptions = 0;

            for symbol in symbols {
                // 小延遲避免訂閱衝突
                tokio::time::sleep(Duration::from_millis(50)).await;

                let orderbook_result = exchange.subscribe_orderbook(symbol, 15).await;
                let trades_result = exchange.subscribe_trades(symbol).await;
                let ticker_result = exchange.subscribe_ticker(symbol).await;

                if orderbook_result.is_ok() && trades_result.is_ok() && ticker_result.is_ok() {
                    successful_subscriptions += 1;
                    info!("✅ [{}:{}] 訂閱成功", exchange_name, symbol);
                } else {
                    warn!("⚠️ [{}:{}] 訂閱失敗", exchange_name, symbol);
                }
            }

            // 更新訂閱狀態
            for (i, result) in results.iter_mut().enumerate() {
                result.subscription_success = i < successful_subscriptions;
            }

            info!(
                "📊 [{}] 訂閱完成: {}/{} 商品成功",
                exchange_name,
                successful_subscriptions,
                symbols.len()
            );
        }

        // 階段3: 統一數據收集
        if results.iter().any(|r| r.subscription_success) {
            if let Ok(mut receiver) = {
                let exchange = exchange_ref.read().await;
                exchange.get_market_events().await
            } {
                info!("📊 [{}] 開始統一數據收集", exchange_name);

                let test_start = Instant::now();
                let test_duration = Duration::from_secs(test_duration_secs);

                // 使用 HashMap 統計各商品事件
                let mut symbol_stats: HashMap<String, (u64, u64, u64)> = HashMap::new();
                for symbol in symbols {
                    symbol_stats.insert(symbol.clone(), (0, 0, 0)); // (orderbook, trade, ticker)
                }

                while test_start.elapsed() < test_duration {
                    match timeout(Duration::from_millis(50), receiver.recv()).await {
                        Ok(Some(event)) => {
                            // 統計各商品的事件
                            match &event {
                                MarketEvent::OrderBookUpdate {
                                    symbol: event_symbol,
                                    ..
                                } => {
                                    if let Some(stats) = symbol_stats.get_mut(event_symbol) {
                                        stats.0 += 1;
                                    }
                                }
                                MarketEvent::Trade {
                                    symbol: event_symbol,
                                    ..
                                } => {
                                    if let Some(stats) = symbol_stats.get_mut(event_symbol) {
                                        stats.1 += 1;
                                    }
                                }
                                MarketEvent::Ticker {
                                    symbol: event_symbol,
                                    ..
                                } => {
                                    if let Some(stats) = symbol_stats.get_mut(event_symbol) {
                                        stats.2 += 1;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(None) => {
                            warn!("📭 [{}] 連接意外關閉", exchange_name);
                            break;
                        }
                        Err(_) => {
                            // 超時，繼續
                        }
                    }
                }

                // 更新各商品的統計結果
                let actual_duration = test_start.elapsed().as_secs_f64();
                for (result, symbol) in results.iter_mut().zip(symbols.iter()) {
                    if let Some((orderbook, trade, ticker)) = symbol_stats.get(symbol) {
                        result.orderbook_events = *orderbook;
                        result.trade_events = *trade;
                        result.ticker_events = *ticker;
                        result.total_events = orderbook + trade + ticker;
                        result.test_duration_secs = actual_duration;
                        result.events_per_second = result.total_events as f64 / actual_duration;

                        info!(
                            "📊 [{}:{}] 收集完成: {} 事件 ({:.1}/秒)",
                            exchange_name, symbol, result.total_events, result.events_per_second
                        );
                    }
                }
            }
        }

        // 階段4: 清理
        {
            let mut exchange = exchange_ref.write().await;
            let _ = exchange.disconnect_public().await;
        }

        results
    }

    /// 測試單個交易所的單個商品 - 完全獨立版本 (解決並發衝突)
    async fn test_single_exchange_symbol_independent_static(
        exchange_name: &str,
        symbol: &str,
        test_duration_secs: u64,
    ) -> SymbolTestResult {
        let mut result = SymbolTestResult {
            symbol: symbol.to_string(),
            exchange: exchange_name.to_string(),
            connection_success: false,
            subscription_success: false,
            total_events: 0,
            orderbook_events: 0,
            trade_events: 0,
            ticker_events: 0,
            events_per_second: 0.0,
            test_duration_secs: 0.0,
        };

        info!("🔗 [{}:{}] 開始獨立測試", exchange_name, symbol);

        // 創建獨立的 ExchangeManager 和交易所實例
        let exchange_manager = Arc::new(ExchangeManager::new());

        // 根據交易所名稱創建對應實例
        let exchange_instance: Box<dyn Exchange + Send + Sync> = match exchange_name {
            "bitget" => Box::new(BitgetExchange::new()),
            "binance" => Box::new(BinanceExchange::new()),
            "bybit" => Box::new(BybitExchange::new()),
            _ => {
                error!("❌ [{}:{}] 未知的交易所類型", exchange_name, symbol);
                return result;
            }
        };

        exchange_manager
            .add_exchange(exchange_name.to_string(), exchange_instance)
            .await;

        // 獲取交易所實例
        let exchange_ref = match exchange_manager.get_exchange(exchange_name).await {
            Some(exchange) => exchange,
            None => {
                error!("❌ [{}:{}] 交易所實例創建失敗", exchange_name, symbol);
                return result;
            }
        };

        // 階段1: 連接測試
        let connection_start = Instant::now();
        {
            let mut exchange = exchange_ref.write().await;
            match timeout(Duration::from_secs(15), exchange.connect_public()).await {
                Ok(Ok(())) => {
                    let connection_time_ms = connection_start.elapsed().as_millis() as f64;
                    result.connection_success = true;
                    info!(
                        "✅ [{}:{}] 連接成功 ({:.0}ms)",
                        exchange_name, symbol, connection_time_ms
                    );
                }
                Ok(Err(e)) => {
                    error!("❌ [{}:{}] 連接失敗: {}", exchange_name, symbol, e);
                    return result;
                }
                Err(_) => {
                    error!("❌ [{}:{}] 連接超時", exchange_name, symbol);
                    return result;
                }
            }
        }

        // 階段2: 訂閱測試
        {
            let mut exchange = exchange_ref.write().await;

            let orderbook_result = exchange.subscribe_orderbook(symbol, 15).await;
            let trades_result = exchange.subscribe_trades(symbol).await;
            let ticker_result = exchange.subscribe_ticker(symbol).await;

            match (orderbook_result, trades_result, ticker_result) {
                (Ok(()), Ok(()), Ok(())) => {
                    result.subscription_success = true;
                    info!("✅ [{}:{}] 所有頻道訂閱成功", exchange_name, symbol);
                }
                _ => {
                    warn!("⚠️ [{}:{}] 部分訂閱失敗", exchange_name, symbol);
                    result.subscription_success = false;
                }
            }
        }

        // 階段3: 性能數據收集
        if result.subscription_success {
            if let Ok(mut receiver) = {
                let exchange = exchange_ref.read().await;
                exchange.get_market_events().await
            } {
                info!("📊 [{}:{}] 開始性能數據收集", exchange_name, symbol);

                let test_start = Instant::now();
                let test_duration = Duration::from_secs(test_duration_secs);

                let mut processing_times = Vec::new();
                let mut last_progress_time = Instant::now();

                while test_start.elapsed() < test_duration {
                    match timeout(Duration::from_millis(50), receiver.recv()).await {
                        Ok(Some(event)) => {
                            let processing_start = Instant::now();

                            // 統計事件類型 (只統計目標商品的事件)
                            match &event {
                                MarketEvent::OrderBookUpdate {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.orderbook_events += 1;
                                }
                                MarketEvent::Trade {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.trade_events += 1;
                                }
                                MarketEvent::Ticker {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.ticker_events += 1;
                                }
                                _ => {
                                    // 忽略其他商品或心跳事件
                                }
                            }

                            // 記錄處理延遲
                            let processing_time = processing_start.elapsed().as_micros() as f64;
                            processing_times.push(processing_time);

                            // 每5秒打印一次進度
                            if last_progress_time.elapsed() >= Duration::from_secs(5) {
                                let elapsed_secs = test_start.elapsed().as_secs_f64();
                                let current_rate = result.total_events as f64 / elapsed_secs;
                                info!(
                                    "📈 [{}:{}] 進度: {} 事件 ({:.1}/秒) [OB:{} T:{} TK:{}]",
                                    exchange_name,
                                    symbol,
                                    result.total_events,
                                    current_rate,
                                    result.orderbook_events,
                                    result.trade_events,
                                    result.ticker_events
                                );
                                last_progress_time = Instant::now();
                            }
                        }
                        Ok(None) => {
                            warn!("📭 [{}:{}] 連接意外關閉", exchange_name, symbol);
                            break;
                        }
                        Err(_) => {
                            // 超時，繼續
                        }
                    }
                }

                // 計算性能指標
                let actual_duration = test_start.elapsed().as_secs_f64();
                result.test_duration_secs = actual_duration;
                result.events_per_second = result.total_events as f64 / actual_duration;

                info!(
                    "📊 [{}:{}] 數據收集完成: {} 事件 ({:.1}/秒)",
                    exchange_name, symbol, result.total_events, result.events_per_second
                );
            }
        }

        // 階段4: 清理
        {
            let mut exchange = exchange_ref.write().await;
            let _ = exchange.disconnect_public().await;
        }

        result
    }

    /// 測試單個交易所的單個商品 - 真正並發版本 (從 concurrent_performance_test 移植)
    async fn test_single_exchange_symbol_concurrent_static(
        exchange_manager: Arc<ExchangeManager>,
        exchange_name: &str,
        symbol: &str,
        test_duration_secs: u64,
    ) -> SymbolTestResult {
        let mut result = SymbolTestResult {
            symbol: symbol.to_string(),
            exchange: exchange_name.to_string(),
            connection_success: false,
            subscription_success: false,
            total_events: 0,
            orderbook_events: 0,
            trade_events: 0,
            ticker_events: 0,
            events_per_second: 0.0,
            test_duration_secs: 0.0,
        };

        info!("🔗 [{}:{}] 開始並發測試", exchange_name, symbol);

        // 獲取交易所實例
        let exchange_ref = match exchange_manager.get_exchange(exchange_name).await {
            Some(exchange) => exchange,
            None => {
                error!("❌ [{}:{}] 交易所實例未找到", exchange_name, symbol);
                return result;
            }
        };

        // 階段1: 連接測試
        let connection_start = Instant::now();
        {
            let mut exchange = exchange_ref.write().await;
            match timeout(Duration::from_secs(15), exchange.connect_public()).await {
                Ok(Ok(())) => {
                    let connection_time_ms = connection_start.elapsed().as_millis() as f64;
                    result.connection_success = true;
                    info!(
                        "✅ [{}:{}] 連接成功 ({:.0}ms)",
                        exchange_name, symbol, connection_time_ms
                    );
                }
                Ok(Err(e)) => {
                    error!("❌ [{}:{}] 連接失敗: {}", exchange_name, symbol, e);
                    return result;
                }
                Err(_) => {
                    error!("❌ [{}:{}] 連接超時", exchange_name, symbol);
                    return result;
                }
            }
        }

        // 階段2: 訂閱測試
        {
            let mut exchange = exchange_ref.write().await;

            let orderbook_result = exchange.subscribe_orderbook(symbol, 15).await;
            let trades_result = exchange.subscribe_trades(symbol).await;
            let ticker_result = exchange.subscribe_ticker(symbol).await;

            match (orderbook_result, trades_result, ticker_result) {
                (Ok(()), Ok(()), Ok(())) => {
                    result.subscription_success = true;
                    info!("✅ [{}:{}] 所有頻道訂閱成功", exchange_name, symbol);
                }
                _ => {
                    warn!("⚠️ [{}:{}] 部分訂閱失敗", exchange_name, symbol);
                    result.subscription_success = false;
                }
            }
        }

        // 階段3: 性能數據收集 (使用concurrent_performance_test的成功方法)
        if result.subscription_success {
            if let Ok(mut receiver) = {
                let exchange = exchange_ref.read().await;
                exchange.get_market_events().await
            } {
                info!("📊 [{}:{}] 開始性能數據收集", exchange_name, symbol);

                let test_start = Instant::now();
                let test_duration = Duration::from_secs(test_duration_secs);

                let mut processing_times = Vec::new();
                let mut last_progress_time = Instant::now();

                while test_start.elapsed() < test_duration {
                    match timeout(Duration::from_millis(50), receiver.recv()).await {
                        Ok(Some(event)) => {
                            let processing_start = Instant::now();

                            // 統計事件類型 (只統計目標商品的事件)
                            match &event {
                                MarketEvent::OrderBookUpdate {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.orderbook_events += 1;
                                }
                                MarketEvent::Trade {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.trade_events += 1;
                                }
                                MarketEvent::Ticker {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.ticker_events += 1;
                                }
                                _ => {
                                    // 忽略其他商品或心跳事件
                                }
                            }

                            // 記錄處理延遲
                            let processing_time = processing_start.elapsed().as_micros() as f64;
                            processing_times.push(processing_time);

                            // 每5秒打印一次進度
                            if last_progress_time.elapsed() >= Duration::from_secs(5) {
                                let elapsed_secs = test_start.elapsed().as_secs_f64();
                                let current_rate = result.total_events as f64 / elapsed_secs;
                                info!(
                                    "📈 [{}:{}] 進度: {} 事件 ({:.1}/秒) [OB:{} T:{} TK:{}]",
                                    exchange_name,
                                    symbol,
                                    result.total_events,
                                    current_rate,
                                    result.orderbook_events,
                                    result.trade_events,
                                    result.ticker_events
                                );
                                last_progress_time = Instant::now();
                            }
                        }
                        Ok(None) => {
                            warn!("📭 [{}:{}] 連接意外關閉", exchange_name, symbol);
                            break;
                        }
                        Err(_) => {
                            // 超時，繼續
                        }
                    }
                }

                // 計算性能指標
                let actual_duration = test_start.elapsed().as_secs_f64();
                result.test_duration_secs = actual_duration;
                result.events_per_second = result.total_events as f64 / actual_duration;

                info!(
                    "📊 [{}:{}] 數據收集完成: {} 事件 ({:.1}/秒)",
                    exchange_name, symbol, result.total_events, result.events_per_second
                );
            }
        }

        // 階段4: 清理
        {
            let mut exchange = exchange_ref.write().await;
            let _ = exchange.disconnect_public().await;
        }

        result
    }

    /// 測試單個交易所的單個商品 (靜態方法) - 舊版本，保留兼容性
    async fn test_single_exchange_symbol_static(
        exchange_manager: Arc<ExchangeManager>,
        exchange_name: &str,
        symbol: &str,
        test_duration_secs: u64,
    ) -> SymbolTestResult {
        let mut result = SymbolTestResult {
            symbol: symbol.to_string(),
            exchange: exchange_name.to_string(),
            connection_success: false,
            subscription_success: false,
            total_events: 0,
            orderbook_events: 0,
            trade_events: 0,
            ticker_events: 0,
            events_per_second: 0.0,
            test_duration_secs: 0.0,
        };

        info!("🔗 [{}:{}] 開始測試", exchange_name, symbol);

        // 獲取交易所實例
        let exchange_ref = match exchange_manager.get_exchange(exchange_name).await {
            Some(exchange) => exchange,
            None => {
                error!("❌ [{}:{}] 交易所實例未找到", exchange_name, symbol);
                return result;
            }
        };

        // 階段1: 連接測試
        {
            let mut exchange = exchange_ref.write().await;
            match timeout(Duration::from_secs(15), exchange.connect_public()).await {
                Ok(Ok(())) => {
                    result.connection_success = true;
                    info!("✅ [{}:{}] 連接成功", exchange_name, symbol);
                }
                Ok(Err(e)) => {
                    error!("❌ [{}:{}] 連接失敗: {}", exchange_name, symbol, e);
                    return result;
                }
                Err(_) => {
                    error!("❌ [{}:{}] 連接超時", exchange_name, symbol);
                    return result;
                }
            }
        }

        // 階段2: 訂閱測試
        {
            let mut exchange = exchange_ref.write().await;

            let orderbook_result = exchange.subscribe_orderbook(symbol, 15).await;
            let trades_result = exchange.subscribe_trades(symbol).await;
            let ticker_result = exchange.subscribe_ticker(symbol).await;

            match (orderbook_result, trades_result, ticker_result) {
                (Ok(()), Ok(()), Ok(())) => {
                    result.subscription_success = true;
                    info!("✅ [{}:{}] 所有頻道訂閱成功", exchange_name, symbol);
                }
                _ => {
                    warn!("⚠️ [{}:{}] 部分訂閱失敗", exchange_name, symbol);
                    result.subscription_success = false;
                }
            }
        }

        // 階段3: 數據收集
        if result.subscription_success {
            if let Ok(mut receiver) = {
                let exchange = exchange_ref.read().await;
                exchange.get_market_events().await
            } {
                info!("📊 [{}:{}] 開始數據收集", exchange_name, symbol);

                let test_start = Instant::now();
                let test_duration = Duration::from_secs(test_duration_secs);

                while test_start.elapsed() < test_duration {
                    match timeout(Duration::from_millis(50), receiver.recv()).await {
                        Ok(Some(event)) => {
                            // 只統計目標商品的事件
                            match &event {
                                MarketEvent::OrderBookUpdate {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.orderbook_events += 1;
                                }
                                MarketEvent::Trade {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.trade_events += 1;
                                }
                                MarketEvent::Ticker {
                                    symbol: event_symbol,
                                    ..
                                } if event_symbol == symbol => {
                                    result.total_events += 1;
                                    result.ticker_events += 1;
                                }
                                _ => {
                                    // 忽略其他商品的事件
                                }
                            }
                        }
                        Ok(None) => {
                            warn!("📭 [{}:{}] 連接意外關閉", exchange_name, symbol);
                            break;
                        }
                        Err(_) => {
                            // 超時，繼續
                        }
                    }
                }

                // 計算性能指標
                result.test_duration_secs = test_start.elapsed().as_secs_f64();
                result.events_per_second = result.total_events as f64 / result.test_duration_secs;

                info!(
                    "📊 [{}:{}] 數據收集完成: {} 事件 ({:.1}/秒)",
                    exchange_name, symbol, result.total_events, result.events_per_second
                );
            }
        }

        // 階段4: 清理
        {
            let mut exchange = exchange_ref.write().await;
            let _ = exchange.disconnect_public().await;
        }

        result
    }

    /// 打印綜合分析報告
    async fn print_comprehensive_analysis(&self, results: &[SymbolTestResult]) {
        info!("");
        info!("📊 多交易所多商品並發測試綜合分析報告");
        info!("===============================================================================");

        // 按交易所分組
        let mut exchange_stats: HashMap<String, Vec<&SymbolTestResult>> = HashMap::new();
        for result in results {
            exchange_stats
                .entry(result.exchange.clone())
                .or_insert_with(Vec::new)
                .push(result);
        }

        // 1. 按交易所分析
        info!("🔹 按交易所分析:");
        for (exchange, exchange_results) in exchange_stats.iter() {
            let successful_symbols = exchange_results
                .iter()
                .filter(|r| r.connection_success && r.subscription_success)
                .count();
            let total_events: u64 = exchange_results.iter().map(|r| r.total_events).sum();
            let avg_rate: f64 = exchange_results
                .iter()
                .map(|r| r.events_per_second)
                .sum::<f64>()
                / exchange_results.len() as f64;

            info!(
                "   📈 {}: {}/{} 商品成功，總事件 {}，平均速率 {:.1}/秒",
                exchange.to_uppercase(),
                successful_symbols,
                exchange_results.len(),
                total_events,
                avg_rate
            );
        }

        // 2. 按商品分析
        info!("");
        info!("🔹 按商品分析:");
        let mut symbol_stats: HashMap<String, Vec<&SymbolTestResult>> = HashMap::new();
        for result in results {
            symbol_stats
                .entry(result.symbol.clone())
                .or_insert_with(Vec::new)
                .push(result);
        }

        for (symbol, symbol_results) in symbol_stats.iter() {
            let successful_exchanges = symbol_results
                .iter()
                .filter(|r| r.connection_success && r.subscription_success)
                .count();
            let total_events: u64 = symbol_results.iter().map(|r| r.total_events).sum();

            info!(
                "   💰 {}: {}/3 交易所成功，總事件 {}",
                symbol, successful_exchanges, total_events
            );
        }

        // 3. 詳細數據表格
        info!("");
        info!("📋 詳細測試結果:");
        info!("--------------------------------------------------------------------------------");
        info!(
            "{:>10} | {:>10} | {:>8} | {:>8} | {:>8} | {:>8} | {:>10}",
            "交易所", "商品", "總事件", "訂單簿", "交易", "Ticker", "速率/秒"
        );
        info!("--------------------------------------------------------------------------------");

        for result in results {
            if result.connection_success && result.subscription_success {
                info!(
                    "{:>10} | {:>10} | {:>8} | {:>8} | {:>8} | {:>8} | {:>10.1}",
                    result.exchange,
                    result.symbol,
                    result.total_events,
                    result.orderbook_events,
                    result.trade_events,
                    result.ticker_events,
                    result.events_per_second
                );
            } else {
                info!(
                    "{:>10} | {:>10} | {:>8} | {:>8} | {:>8} | {:>8} | {:>10}",
                    result.exchange, result.symbol, "失敗", "N/A", "N/A", "N/A", "N/A"
                );
            }
        }

        // 4. 系統總結
        info!("");
        info!("🔗 系統並發性能總結:");

        let successful_tests = results
            .iter()
            .filter(|r| r.connection_success && r.subscription_success)
            .count();
        let total_events: u64 = results.iter().map(|r| r.total_events).sum();
        let total_rate: f64 = results.iter().map(|r| r.events_per_second).sum();

        info!(
            "   成功測試: {}/{} ({:.1}%)",
            successful_tests,
            results.len(),
            (successful_tests as f64 / results.len() as f64) * 100.0
        );
        info!("   系統總事件處理: {} 事件", total_events);
        info!("   系統總處理速率: {:.1} 事件/秒", total_rate);
        info!(
            "   並發連接數: {} 個交易所 × {} 個商品",
            exchange_stats.len(),
            self.test_symbols.len()
        );

        // 5. 性能評估
        if total_rate > 200.0 {
            info!("   🎉 並發性能評估: 優秀 (>200 事件/秒)");
        } else if total_rate > 100.0 {
            info!("   ✅ 並發性能評估: 良好 (>100 事件/秒)");
        } else if total_rate > 50.0 {
            info!("   ⚠️ 並發性能評估: 一般 (>50 事件/秒)");
        } else {
            warn!("   ❌ 並發性能評估: 需要優化 (<50 事件/秒)");
        }

        info!("===============================================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("test_all_exchanges=info,rust_hft=info")
        .init();

    info!("🚀 開始多交易所多商品並發測試");

    // 定義測試商品列表 - 主流加密貨幣交易對
    let test_symbols = vec![
        "BTCUSDT".to_string(),
        "ETHUSDT".to_string(),
        "BNBUSDT".to_string(),
        "ADAUSDT".to_string(),
        "SOLUSDT".to_string(),
    ];

    // 創建測試框架
    let test_framework = MultiExchangeMultiSymbolTest::new(
        test_symbols,
        20, // 20秒測試時間
    )
    .await?;

    // 運行綜合測試
    match test_framework.run_comprehensive_test().await {
        Ok(results) => {
            let successful_count = results
                .iter()
                .filter(|r| r.connection_success && r.subscription_success)
                .count();

            info!("");
            if successful_count == results.len() {
                info!("🎉 多交易所多商品測試完全成功!");
                info!("   所有 {} 個測試組合都正常運行", results.len());

                let total_rate: f64 = results
                    .iter()
                    .filter(|r| r.connection_success && r.subscription_success)
                    .map(|r| r.events_per_second)
                    .sum();

                println!("✅ 系統多商品並發處理能力: {:.1} 事件/秒", total_rate);
            } else {
                warn!("⚠️ 部分測試成功 ({}/{})", successful_count, results.len());
                println!("⚠️ 部分交易所或商品存在問題，建議檢查配置");
            }
        }
        Err(e) => {
            error!("❌ 多交易所多商品測試失敗: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
// Archived legacy example
