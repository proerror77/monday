/*!
 * 深度對比測試 - 測試不同深度配置下各交易所的事件接收率
 *
 * 功能：
 * 1. 測試 1, 5, 10, 15, 20 檔深度配置
 * 2. 比較各交易所在不同深度下的事件數量
 * 3. 找出 Bitget 事件偏低的根本原因
 * 4. 提供優化建議
 */

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tracing::{debug, error, info, warn};

use rust_hft::exchanges::message_types::MarketEvent;
use rust_hft::exchanges::{BinanceExchange, BitgetExchange, BybitExchange, ExchangeManager};

/// 深度測試結果
#[derive(Debug, Clone)]
pub struct DepthTestResult {
    pub exchange: String,
    pub depth: u32,
    pub total_events: u64,
    pub orderbook_events: u64,
    pub trade_events: u64,
    pub ticker_events: u64,
    pub events_per_second: f64,
    pub connection_success: bool,
    pub subscription_success: bool,
}

/// 深度對比測試框架
pub struct DepthComparisonTest {
    test_symbol: String,
    test_duration_secs: u64,
    depths_to_test: Vec<u32>,
}

impl DepthComparisonTest {
    /// 創建新的深度測試框架
    pub fn new(test_symbol: String, test_duration_secs: u64) -> Self {
        Self {
            test_symbol,
            test_duration_secs,
            depths_to_test: vec![1, 5, 10, 15, 20], // 測試標準深度
        }
    }

    /// 運行完整的深度對比測試
    pub async fn run_depth_comparison(&self) -> Result<Vec<DepthTestResult>> {
        info!("🎯 開始深度對比測試");
        info!("   測試交易對: {}", self.test_symbol);
        info!(
            "   測試時長: {} 秒 x {} 個深度",
            self.test_duration_secs,
            self.depths_to_test.len()
        );
        info!("   測試深度: {:?}", self.depths_to_test);

        let exchanges = ["bitget", "binance", "bybit"];
        let mut all_results = Vec::new();

        for depth in &self.depths_to_test {
            info!("");
            info!("📊 測試深度: {} 檔", depth);
            info!(
                "================================================================================"
            );

            for exchange_name in &exchanges {
                let result = self.test_exchange_depth(exchange_name, *depth).await;
                all_results.push(result);
            }
        }

        self.print_comprehensive_analysis(&all_results).await;
        Ok(all_results)
    }

    /// 測試單個交易所在指定深度下的表現
    async fn test_exchange_depth(&self, exchange_name: &str, depth: u32) -> DepthTestResult {
        let mut result = DepthTestResult {
            exchange: exchange_name.to_string(),
            depth,
            total_events: 0,
            orderbook_events: 0,
            trade_events: 0,
            ticker_events: 0,
            events_per_second: 0.0,
            connection_success: false,
            subscription_success: false,
        };

        info!("🔗 [{}] 深度 {} 檔測試開始", exchange_name, depth);

        // 創建交易所管理器
        let exchange_manager = Arc::new(ExchangeManager::new());

        // 添加對應的交易所
        match exchange_name {
            "bitget" => {
                let exchange = Box::new(BitgetExchange::new());
                exchange_manager
                    .add_exchange(exchange_name.to_string(), exchange)
                    .await;
            }
            "binance" => {
                let exchange = Box::new(BinanceExchange::new());
                exchange_manager
                    .add_exchange(exchange_name.to_string(), exchange)
                    .await;
            }
            "bybit" => {
                let exchange = Box::new(BybitExchange::new());
                exchange_manager
                    .add_exchange(exchange_name.to_string(), exchange)
                    .await;
            }
            _ => {
                error!("❌ [{}] 未知的交易所", exchange_name);
                return result;
            }
        }

        if let Some(exchange_ref) = exchange_manager.get_exchange(exchange_name).await {
            // 連接和訂閱
            {
                let mut exchange = exchange_ref.write().await;

                // 測試連接
                match timeout(Duration::from_secs(10), exchange.connect_public()).await {
                    Ok(Ok(())) => {
                        result.connection_success = true;
                        debug!("✅ [{}] 深度 {} 連接成功", exchange_name, depth);

                        // 訂閱市場數據（使用指定深度）
                        let orderbook_result =
                            exchange.subscribe_orderbook(&self.test_symbol, depth).await;
                        let trades_result = exchange.subscribe_trades(&self.test_symbol).await;
                        let ticker_result = exchange.subscribe_ticker(&self.test_symbol).await;

                        match (orderbook_result, trades_result, ticker_result) {
                            (Ok(()), Ok(()), Ok(())) => {
                                result.subscription_success = true;
                                debug!("✅ [{}] 深度 {} 訂閱成功", exchange_name, depth);
                            }
                            _ => {
                                warn!("⚠️ [{}] 深度 {} 部分訂閱失敗", exchange_name, depth);
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("❌ [{}] 深度 {} 連接失敗: {}", exchange_name, depth, e);
                        return result;
                    }
                    Err(_) => {
                        error!("❌ [{}] 深度 {} 連接超時", exchange_name, depth);
                        return result;
                    }
                }
            }

            // 收集數據
            if result.subscription_success {
                if let Ok(mut receiver) = {
                    let exchange = exchange_ref.read().await;
                    exchange.get_market_events().await
                } {
                    let test_start = Instant::now();
                    let test_duration = Duration::from_secs(self.test_duration_secs);

                    debug!("📡 [{}] 深度 {} 開始數據收集", exchange_name, depth);

                    while test_start.elapsed() < test_duration {
                        match timeout(Duration::from_millis(50), receiver.recv()).await {
                            Ok(Some(event)) => {
                                result.total_events += 1;

                                // 分類事件類型
                                match event {
                                    MarketEvent::OrderBookUpdate { .. } => {
                                        result.orderbook_events += 1
                                    }
                                    MarketEvent::Trade { .. } => result.trade_events += 1,
                                    MarketEvent::Ticker { .. } => result.ticker_events += 1,
                                    _ => {}
                                }
                            }
                            Ok(None) => break,
                            Err(_) => {} // 超時，繼續
                        }
                    }

                    // 計算平均每秒事件數
                    let actual_duration = test_start.elapsed().as_secs_f64();
                    result.events_per_second = result.total_events as f64 / actual_duration;

                    info!(
                        "📊 [{}] 深度 {}: {} 事件 ({:.1}/秒) [OB:{} T:{} TK:{}]",
                        exchange_name,
                        depth,
                        result.total_events,
                        result.events_per_second,
                        result.orderbook_events,
                        result.trade_events,
                        result.ticker_events
                    );
                }

                // 斷開連接
                {
                    let mut exchange = exchange_ref.write().await;
                    let _ = exchange.disconnect_public().await;
                }
            }
        }

        result
    }

    /// 打印完整分析報告
    async fn print_comprehensive_analysis(&self, results: &[DepthTestResult]) {
        info!("");
        info!("📈 深度對比測試 - 完整分析報告");
        info!("================================================================================");

        // 按交易所分組結果
        let mut exchange_results: HashMap<String, Vec<&DepthTestResult>> = HashMap::new();
        for result in results {
            exchange_results
                .entry(result.exchange.clone())
                .or_insert_with(Vec::new)
                .push(result);
        }

        // 1. 按深度顯示比較
        info!("📊 按深度比較 (事件/秒):");
        info!("--------------------------------------------------------------------------------");
        info!(
            "{:8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8}",
            "深度", "Bitget", "Binance", "Bybit", "最高", "Bitget比例"
        );
        info!("--------------------------------------------------------------------------------");

        for depth in &self.depths_to_test {
            let bitget_rate = results
                .iter()
                .find(|r| r.exchange == "bitget" && r.depth == *depth)
                .map(|r| r.events_per_second)
                .unwrap_or(0.0);

            let binance_rate = results
                .iter()
                .find(|r| r.exchange == "binance" && r.depth == *depth)
                .map(|r| r.events_per_second)
                .unwrap_or(0.0);

            let bybit_rate = results
                .iter()
                .find(|r| r.exchange == "bybit" && r.depth == *depth)
                .map(|r| r.events_per_second)
                .unwrap_or(0.0);

            let max_rate = bitget_rate.max(binance_rate).max(bybit_rate);
            let bitget_ratio = if max_rate > 0.0 {
                (bitget_rate / max_rate) * 100.0
            } else {
                0.0
            };

            info!(
                "{:8} | {:8.1} | {:8.1} | {:8.1} | {:8.1} | {:7.1}%",
                depth, bitget_rate, binance_rate, bybit_rate, max_rate, bitget_ratio
            );
        }

        // 2. 各交易所深度表現趨勢
        info!("");
        info!("📊 各交易所深度表現趨勢:");
        info!("--------------------------------------------------------------------------------");

        for (exchange_name, exchange_results) in &exchange_results {
            info!("🔹 {} 表現:", exchange_name);

            let mut sorted_results = exchange_results.clone();
            sorted_results.sort_by_key(|r| r.depth);

            for result in sorted_results {
                let ob_percent = if result.total_events > 0 {
                    result.orderbook_events as f64 / result.total_events as f64 * 100.0
                } else {
                    0.0
                };
                let trade_percent = if result.total_events > 0 {
                    result.trade_events as f64 / result.total_events as f64 * 100.0
                } else {
                    0.0
                };
                let ticker_percent = if result.total_events > 0 {
                    result.ticker_events as f64 / result.total_events as f64 * 100.0
                } else {
                    0.0
                };

                info!("   深度 {:2}: {:4} 事件 ({:5.1}/秒) [訂單簿:{:4.1}% 交易:{:4.1}% Ticker:{:4.1}%]", 
                    result.depth, result.total_events, result.events_per_second,
                    ob_percent, trade_percent, ticker_percent);
            }
        }

        // 3. Bitget 問題診斷
        info!("");
        info!("🔍 Bitget 問題診斷:");
        info!("--------------------------------------------------------------------------------");

        let bitget_results: Vec<&DepthTestResult> =
            results.iter().filter(|r| r.exchange == "bitget").collect();

        if !bitget_results.is_empty() {
            let avg_bitget_rate = bitget_results
                .iter()
                .map(|r| r.events_per_second)
                .sum::<f64>()
                / bitget_results.len() as f64;

            let avg_binance_rate = results
                .iter()
                .filter(|r| r.exchange == "binance")
                .map(|r| r.events_per_second)
                .sum::<f64>()
                / results.iter().filter(|r| r.exchange == "binance").count() as f64;

            let avg_bybit_rate = results
                .iter()
                .filter(|r| r.exchange == "bybit")
                .map(|r| r.events_per_second)
                .sum::<f64>()
                / results.iter().filter(|r| r.exchange == "bybit").count() as f64;

            info!("平均事件率:");
            info!("   Bitget: {:.1} 事件/秒", avg_bitget_rate);
            info!("   Binance: {:.1} 事件/秒", avg_binance_rate);
            info!("   Bybit: {:.1} 事件/秒", avg_bybit_rate);

            let performance_gap = ((avg_binance_rate + avg_bybit_rate) / 2.0 - avg_bitget_rate)
                / ((avg_binance_rate + avg_bybit_rate) / 2.0)
                * 100.0;

            if performance_gap > 30.0 {
                warn!("⚠️ Bitget 事件率比其他交易所低 {:.1}%", performance_gap);
                warn!("可能原因:");
                warn!("   1. 訂閱頻道配置不是最優的");
                warn!("   2. WebSocket 消息解析遺漏某些事件類型");
                warn!("   3. Bitget API 的更新頻率本身較低");
                warn!("   4. 網絡連接延遲或質量問題");

                info!("");
                info!("💡 建議修復措施:");
                info!("   1. 檢查 Bitget 是否有更高頻的訂閱頻道 (如 books1 vs books15)");
                info!("   2. 添加 WebSocket 消息原始日誌輸出進行診斷");
                info!("   3. 嘗試訂閱更多類型的市場數據頻道");
                info!("   4. 與 Bitget 官方文檔對比，確保 API 使用正確");
            } else {
                info!(
                    "✅ Bitget 事件率在正常範圍內 (差距: {:.1}%)",
                    performance_gap
                );
            }
        }

        info!("================================================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info,depth_comparison_test=info")
        .init();

    info!("🚀 開始深度對比測試");

    // 創建測試框架
    let test_framework = DepthComparisonTest::new(
        "BTCUSDT".to_string(),
        8, // 每個深度測試8秒
    );

    // 運行深度對比測試
    match test_framework.run_depth_comparison().await {
        Ok(results) => {
            let total_tests = results.len();
            let successful_tests = results
                .iter()
                .filter(|r| r.connection_success && r.subscription_success)
                .count();

            info!("");
            info!("🎉 深度對比測試完成!");
            info!("   成功測試: {}/{} 個配置", successful_tests, total_tests);

            if successful_tests == total_tests {
                info!("✅ 所有深度配置測試成功");
            } else {
                warn!("⚠️ 部分深度配置測試失敗");
            }
        }
        Err(e) => {
            error!("❌ 深度對比測試失敗: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
// Archived legacy example; see grouped examples
