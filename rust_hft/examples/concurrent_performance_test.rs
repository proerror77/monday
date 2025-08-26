/*!
 * 並發性能測試 - 測試三個交易所的並發市場數據接收性能
 *
 * 功能：
 * 1. 同時連接 Bitget、Binance、Bybit 三個交易所
 * 2. 並發訂閱市場數據並統計性能指標
 * 3. 實時比較各交易所的事件接收率
 * 4. 測試並發處理能力和系統資源使用
 */

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tracing::{error, info, warn};

use rust_hft::exchanges::message_types::MarketEvent;
use rust_hft::exchanges::{BinanceExchange, BitgetExchange, BybitExchange, ExchangeManager};

/// 並發性能測試結果
#[derive(Debug, Clone)]
pub struct ConcurrentPerformanceResult {
    pub exchange: String,
    pub connection_time_ms: f64,
    pub total_events: u64,
    pub orderbook_events: u64,
    pub trade_events: u64,
    pub ticker_events: u64,
    pub heartbeat_events: u64,
    pub events_per_second: f64,
    pub avg_processing_latency_us: f64,
    pub max_processing_latency_us: f64,
    pub connection_stable: bool,
    pub subscription_success: bool,
}

/// 並發性能測試框架
pub struct ConcurrentPerformanceTest {
    test_symbol: String,
    test_duration_secs: u64,
    exchange_manager: Arc<ExchangeManager>,
}

impl ConcurrentPerformanceTest {
    /// 創建新的並發性能測試框架
    pub async fn new(test_symbol: String, test_duration_secs: u64) -> Result<Self> {
        let exchange_manager = Arc::new(ExchangeManager::new());

        // 添加三個交易所
        info!("🏗️ 初始化交易所實例");

        let bitget_exchange = Box::new(BitgetExchange::new());
        exchange_manager
            .add_exchange("bitget".to_string(), bitget_exchange)
            .await;

        let binance_exchange = Box::new(BinanceExchange::new());
        exchange_manager
            .add_exchange("binance".to_string(), binance_exchange)
            .await;

        let bybit_exchange = Box::new(BybitExchange::new());
        exchange_manager
            .add_exchange("bybit".to_string(), bybit_exchange)
            .await;

        info!("✅ 交易所實例初始化完成");

        Ok(Self {
            test_symbol,
            test_duration_secs,
            exchange_manager,
        })
    }

    /// 運行並發性能測試
    pub async fn run_concurrent_performance_test(
        &self,
    ) -> Result<Vec<ConcurrentPerformanceResult>> {
        info!("🚀 開始並發性能測試");
        info!("   測試交易對: {}", self.test_symbol);
        info!("   測試時長: {} 秒", self.test_duration_secs);
        info!("   測試模式: 並發三個交易所");

        let exchanges = ["bitget", "binance", "bybit"];
        let mut test_handles = Vec::new();

        // 並發啟動所有交易所的測試
        for exchange_name in &exchanges {
            let exchange_manager = self.exchange_manager.clone();
            let test_symbol = self.test_symbol.clone();
            let test_duration_secs = self.test_duration_secs;
            let exchange_name = exchange_name.to_string();

            let handle = tokio::spawn(async move {
                Self::test_single_exchange_performance_static(
                    exchange_manager,
                    &exchange_name,
                    &test_symbol,
                    test_duration_secs,
                )
                .await
            });
            test_handles.push(handle);
        }

        // 等待所有測試完成
        let results = futures::future::join_all(test_handles).await;
        let results: Vec<ConcurrentPerformanceResult> =
            results.into_iter().filter_map(|r| r.ok()).collect();

        // 打印並發性能分析
        self.print_concurrent_analysis(&results).await;

        Ok(results)
    }

    /// 測試單個交易所的並發性能 (靜態方法)
    async fn test_single_exchange_performance_static(
        exchange_manager: Arc<ExchangeManager>,
        exchange_name: &str,
        test_symbol: &str,
        test_duration_secs: u64,
    ) -> ConcurrentPerformanceResult {
        let mut result = ConcurrentPerformanceResult {
            exchange: exchange_name.to_string(),
            connection_time_ms: 0.0,
            total_events: 0,
            orderbook_events: 0,
            trade_events: 0,
            ticker_events: 0,
            heartbeat_events: 0,
            events_per_second: 0.0,
            avg_processing_latency_us: 0.0,
            max_processing_latency_us: 0.0,
            connection_stable: false,
            subscription_success: false,
        };

        info!("🔗 [{}] 開始並發性能測試", exchange_name);

        // 獲取交易所實例
        let exchange_ref = match exchange_manager.get_exchange(exchange_name).await {
            Some(exchange) => exchange,
            None => {
                error!("❌ [{}] 交易所實例未找到", exchange_name);
                return result;
            }
        };

        // 階段1: 連接測試
        let connection_start = Instant::now();
        {
            let mut exchange = exchange_ref.write().await;
            match timeout(Duration::from_secs(15), exchange.connect_public()).await {
                Ok(Ok(())) => {
                    result.connection_time_ms = connection_start.elapsed().as_millis() as f64;
                    result.connection_stable = true;
                    info!(
                        "✅ [{}] 連接成功 ({:.0}ms)",
                        exchange_name, result.connection_time_ms
                    );
                }
                Ok(Err(e)) => {
                    error!("❌ [{}] 連接失敗: {}", exchange_name, e);
                    return result;
                }
                Err(_) => {
                    error!("❌ [{}] 連接超時", exchange_name);
                    return result;
                }
            }
        }

        // 階段2: 訂閱測試
        {
            let mut exchange = exchange_ref.write().await;

            let orderbook_result = exchange.subscribe_orderbook(test_symbol, 15).await;
            let trades_result = exchange.subscribe_trades(test_symbol).await;
            let ticker_result = exchange.subscribe_ticker(test_symbol).await;

            match (orderbook_result, trades_result, ticker_result) {
                (Ok(()), Ok(()), Ok(())) => {
                    result.subscription_success = true;
                    info!("✅ [{}] 所有頻道訂閱成功", exchange_name);
                }
                _ => {
                    warn!("⚠️ [{}] 部分訂閱失敗", exchange_name);
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
                info!("📊 [{}] 開始性能數據收集", exchange_name);

                let test_start = Instant::now();
                let test_duration = Duration::from_secs(test_duration_secs);

                let mut processing_times = Vec::new();
                let mut last_progress_time = Instant::now();

                while test_start.elapsed() < test_duration {
                    match timeout(Duration::from_millis(50), receiver.recv()).await {
                        Ok(Some(event)) => {
                            let processing_start = Instant::now();

                            // 統計事件類型
                            result.total_events += 1;
                            match &event {
                                MarketEvent::OrderBookUpdate { .. } => result.orderbook_events += 1,
                                MarketEvent::Trade { .. } => result.trade_events += 1,
                                MarketEvent::Ticker { .. } => result.ticker_events += 1,
                                MarketEvent::Heartbeat { .. } => result.heartbeat_events += 1,
                                _ => {}
                            }

                            // 記錄處理延遲
                            let processing_time = processing_start.elapsed().as_micros() as f64;
                            processing_times.push(processing_time);

                            // 每5秒打印一次進度
                            if last_progress_time.elapsed() >= Duration::from_secs(5) {
                                let elapsed_secs = test_start.elapsed().as_secs_f64();
                                let current_rate = result.total_events as f64 / elapsed_secs;
                                info!(
                                    "📈 [{}] 進度: {} 事件 ({:.1}/秒) [OB:{} T:{} TK:{}]",
                                    exchange_name,
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
                            warn!("📭 [{}] 連接意外關閉", exchange_name);
                            result.connection_stable = false;
                            break;
                        }
                        Err(_) => {
                            // 超時，繼續
                        }
                    }
                }

                // 計算性能指標
                let actual_duration = test_start.elapsed().as_secs_f64();
                result.events_per_second = result.total_events as f64 / actual_duration;

                if !processing_times.is_empty() {
                    result.avg_processing_latency_us =
                        processing_times.iter().sum::<f64>() / processing_times.len() as f64;
                    result.max_processing_latency_us =
                        processing_times.iter().fold(0.0, |max, &val| max.max(val));
                }

                info!(
                    "📊 [{}] 數據收集完成: {} 事件 ({:.1}/秒)",
                    exchange_name, result.total_events, result.events_per_second
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

    /// 打印並發性能分析報告
    async fn print_concurrent_analysis(&self, results: &[ConcurrentPerformanceResult]) {
        info!("");
        info!("📊 並發性能測試分析報告");
        info!("===============================================================================");

        // 個別交易所性能
        for result in results {
            info!("🔹 {}", result.exchange.to_uppercase());

            if result.connection_stable && result.subscription_success {
                info!("   ✅ 狀態: 正常運行");
                info!("   ⏱️ 連接時間: {:.0}ms", result.connection_time_ms);
                info!("   📊 總事件數: {}", result.total_events);
                info!("   📈 事件速率: {:.1} 事件/秒", result.events_per_second);
                info!("   📋 事件分佈:");
                info!(
                    "      • 訂單簿: {} ({:.1}%)",
                    result.orderbook_events,
                    (result.orderbook_events as f64 / result.total_events.max(1) as f64) * 100.0
                );
                info!(
                    "      • 交易: {} ({:.1}%)",
                    result.trade_events,
                    (result.trade_events as f64 / result.total_events.max(1) as f64) * 100.0
                );
                info!(
                    "      • Ticker: {} ({:.1}%)",
                    result.ticker_events,
                    (result.ticker_events as f64 / result.total_events.max(1) as f64) * 100.0
                );
                info!("   ⚡ 處理延遲:");
                info!("      • 平均: {:.2} μs", result.avg_processing_latency_us);
                info!("      • 最大: {:.2} μs", result.max_processing_latency_us);
            } else {
                error!("   ❌ 狀態: 測試失敗");
                if !result.connection_stable {
                    error!("      連接不穩定");
                }
                if !result.subscription_success {
                    error!("      訂閱失敗");
                }
            }
            info!("");
        }

        // 並發性能對比
        info!("📈 並發性能對比:");
        info!("--------------------------------------------------------------------------------");
        info!(
            "{:>10} | {:>12} | {:>12} | {:>12} | {:>12}",
            "交易所", "事件/秒", "總事件", "平均延遲μs", "連接時間ms"
        );
        info!("--------------------------------------------------------------------------------");

        for result in results {
            if result.connection_stable && result.subscription_success {
                info!(
                    "{:>10} | {:>12.1} | {:>12} | {:>12.2} | {:>12.0}",
                    result.exchange,
                    result.events_per_second,
                    result.total_events,
                    result.avg_processing_latency_us,
                    result.connection_time_ms
                );
            } else {
                info!(
                    "{:>10} | {:>12} | {:>12} | {:>12} | {:>12}",
                    result.exchange, "失敗", "N/A", "N/A", "N/A"
                );
            }
        }

        // 系統並發性能總結
        info!("");
        info!("🔗 系統並發性能總結:");

        let successful_results: Vec<_> = results
            .iter()
            .filter(|r| r.connection_stable && r.subscription_success)
            .collect();

        if !successful_results.is_empty() {
            let total_events: u64 = successful_results.iter().map(|r| r.total_events).sum();
            let total_rate: f64 = successful_results.iter().map(|r| r.events_per_second).sum();
            let avg_latency = successful_results
                .iter()
                .map(|r| r.avg_processing_latency_us)
                .sum::<f64>()
                / successful_results.len() as f64;
            let max_latency = successful_results
                .iter()
                .map(|r| r.max_processing_latency_us)
                .fold(0.0f64, |max, val| max.max(val));

            info!("   並發交易所數量: {}", successful_results.len());
            info!("   系統總事件處理: {} 事件", total_events);
            info!("   系統總處理速率: {:.1} 事件/秒", total_rate);
            info!("   系統平均延遲: {:.2} μs", avg_latency);
            info!("   系統最大延遲: {:.2} μs", max_latency);

            // 性能評估
            if total_rate > 150.0 {
                info!("   🎉 性能評估: 優秀 (>150 事件/秒)");
            } else if total_rate > 100.0 {
                info!("   ✅ 性能評估: 良好 (>100 事件/秒)");
            } else if total_rate > 50.0 {
                info!("   ⚠️ 性能評估: 一般 (>50 事件/秒)");
            } else {
                warn!("   ❌ 性能評估: 需要優化 (<50 事件/秒)");
            }

            if avg_latency < 100.0 {
                info!("   ⚡ 延遲評估: 優秀 (<100μs)");
            } else if avg_latency < 500.0 {
                info!("   ⚡ 延遲評估: 良好 (<500μs)");
            } else {
                warn!("   ⚡ 延遲評估: 需要優化 (>500μs)");
            }
        } else {
            error!("   ❌ 沒有成功的並發連接");
        }

        info!("===============================================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("concurrent_performance_test=info,rust_hft=info")
        .init();

    info!("🚀 開始並發性能測試");

    // 創建測試框架
    let test_framework = ConcurrentPerformanceTest::new(
        "BTCUSDT".to_string(),
        30, // 30秒並發測試
    )
    .await?;

    // 運行並發性能測試
    match test_framework.run_concurrent_performance_test().await {
        Ok(results) => {
            let successful_count = results
                .iter()
                .filter(|r| r.connection_stable && r.subscription_success)
                .count();

            info!("");
            if successful_count == results.len() {
                info!("🎉 並發性能測試完全成功!");
                info!("   所有 {} 個交易所都正常運行", results.len());

                let total_rate: f64 = results
                    .iter()
                    .filter(|r| r.connection_stable && r.subscription_success)
                    .map(|r| r.events_per_second)
                    .sum();

                println!("✅ 系統並發處理能力: {:.1} 事件/秒", total_rate);
            } else {
                warn!(
                    "⚠️ 部分並發測試成功 ({}/{})",
                    successful_count,
                    results.len()
                );
                println!("⚠️ 部分交易所存在問題，建議檢查配置");
            }
        }
        Err(e) => {
            error!("❌ 並發性能測試失敗: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
