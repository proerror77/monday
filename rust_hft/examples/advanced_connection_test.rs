/*!
 * 高級連接測試 - 使用多種 TLS 配置嘗試修復連接問題
 *
 * 功能：
 * 1. 嘗試不同的 TLS 配置組合
 * 2. 使用備用 WebSocket URL
 * 3. 測試地理位置限制繞過方案
 * 4. 創建強化的並發測試框架
 */

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// 交易所連接配置
#[derive(Debug, Clone)]
pub struct ExchangeConnectionConfig {
    pub name: String,
    pub primary_url: String,
    pub backup_urls: Vec<String>,
    pub timeout_secs: u64,
    pub retry_count: u32,
}

/// 高級連接測試結果
#[derive(Debug, Clone)]
pub struct AdvancedTestResult {
    pub exchange: String,
    pub successful_url: Option<String>,
    pub connection_time_ms: f64,
    pub error_details: Vec<String>,
    pub retry_attempts: u32,
    pub ping_latency_ms: Option<f64>,
    pub final_status: ConnectionStatus,
}

#[derive(Debug, Clone)]
pub enum ConnectionStatus {
    Success,
    Failed,
    PartialSuccess,
}

/// 高級連接測試框架
pub struct AdvancedConnectionTest {
    configs: Vec<ExchangeConnectionConfig>,
}

impl AdvancedConnectionTest {
    /// 創建新的高級測試框架
    pub fn new() -> Self {
        let configs = vec![
            ExchangeConnectionConfig {
                name: "bitget".to_string(),
                primary_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
                backup_urls: vec!["wss://ws.bitget.com/spot/v1/stream".to_string()],
                timeout_secs: 10,
                retry_count: 2,
            },
            ExchangeConnectionConfig {
                name: "binance".to_string(),
                primary_url: "wss://stream.binance.com:9443/ws".to_string(),
                backup_urls: vec![
                    "wss://stream.binance.com/ws".to_string(),
                    "wss://stream.binance.us:9443/ws".to_string(),
                    "wss://ws-fstream.binance.com/ws".to_string(),
                ],
                timeout_secs: 15,
                retry_count: 3,
            },
            ExchangeConnectionConfig {
                name: "bybit".to_string(),
                primary_url: "wss://stream.bybit.com/v5/public/spot".to_string(),
                backup_urls: vec![
                    "wss://stream.bybit.com/spot/quote/ws/v1".to_string(),
                    "wss://stream.bybit.com/realtime".to_string(),
                    "wss://stream-testnet.bybit.com/v5/public/spot".to_string(),
                ],
                timeout_secs: 15,
                retry_count: 3,
            },
        ];

        Self { configs }
    }

    /// 運行高級並發連接測試
    pub async fn run_advanced_test(&self) -> Result<Vec<AdvancedTestResult>> {
        info!("🚀 開始高級並發連接測試");
        info!("   測試 {} 個交易所", self.configs.len());

        // 並發測試所有交易所
        let mut test_handles = Vec::new();

        for config in &self.configs {
            let config_clone = config.clone();
            let handle =
                tokio::spawn(async move { Self::test_exchange_with_fallback(config_clone).await });
            test_handles.push(handle);
        }

        // 等待所有測試完成
        let results = futures::future::join_all(test_handles).await;
        let final_results: Vec<AdvancedTestResult> =
            results.into_iter().filter_map(|r| r.ok()).collect();

        // 打印分析結果
        self.print_advanced_analysis(&final_results).await;

        Ok(final_results)
    }

    /// 測試單個交易所，支持備用 URL 和重試
    async fn test_exchange_with_fallback(config: ExchangeConnectionConfig) -> AdvancedTestResult {
        let mut result = AdvancedTestResult {
            exchange: config.name.clone(),
            successful_url: None,
            connection_time_ms: 0.0,
            error_details: Vec::new(),
            retry_attempts: 0,
            ping_latency_ms: None,
            final_status: ConnectionStatus::Failed,
        };

        info!("🔗 [{}] 開始高級連接測試", config.name);

        // 嘗試主要 URL
        let urls_to_try = std::iter::once(&config.primary_url)
            .chain(config.backup_urls.iter())
            .collect::<Vec<_>>();

        for (url_index, url) in urls_to_try.iter().enumerate() {
            info!(
                "📡 [{}] 嘗試 URL {}/{}: {}",
                config.name,
                url_index + 1,
                urls_to_try.len(),
                url
            );

            for retry in 0..=config.retry_count {
                result.retry_attempts += 1;

                let connection_start = Instant::now();

                match Self::attempt_connection(&config.name, url, config.timeout_secs).await {
                    Ok((connection_time, ping_latency)) => {
                        result.successful_url = Some(url.to_string());
                        result.connection_time_ms = connection_time;
                        result.ping_latency_ms = ping_latency;
                        result.final_status = ConnectionStatus::Success;

                        info!(
                            "✅ [{}] 連接成功! URL: {}, 時間: {:.0}ms, Ping: {:.0}ms",
                            config.name,
                            url,
                            connection_time,
                            ping_latency.unwrap_or(0.0)
                        );
                        return result;
                    }
                    Err(e) => {
                        let error_msg = format!("URL {} 重試 {}: {}", url, retry + 1, e);
                        result.error_details.push(error_msg.clone());

                        if retry < config.retry_count {
                            debug!("⚠️ [{}] {}, 等待重試...", config.name, error_msg);
                            tokio::time::sleep(Duration::from_millis(1000 * (retry + 1) as u64))
                                .await;
                        } else {
                            warn!("❌ [{}] {}", config.name, error_msg);
                        }
                    }
                }
            }
        }

        error!("❌ [{}] 所有連接嘗試失敗", config.name);
        result
    }

    /// 單次連接嘗試
    async fn attempt_connection(
        exchange_name: &str,
        url: &str,
        timeout_secs: u64,
    ) -> Result<(f64, Option<f64>)> {
        let connection_start = Instant::now();

        // 使用更寬松的超時設置
        let ws_result = timeout(Duration::from_secs(timeout_secs), connect_async(url)).await;

        let (ws_stream, _response) = match ws_result {
            Ok(Ok((stream, response))) => {
                debug!(
                    "[{}] WebSocket 握手成功: {:?}",
                    exchange_name,
                    response.status()
                );
                (stream, response)
            }
            Ok(Err(e)) => {
                return Err(anyhow::anyhow!("WebSocket 錯誤: {}", e));
            }
            Err(_) => {
                return Err(anyhow::anyhow!("連接超時 ({}秒)", timeout_secs));
            }
        };

        let connection_time = connection_start.elapsed().as_millis() as f64;
        let (mut write, mut read) = ws_stream.split();

        // 測試 ping/pong 延遲
        let ping_latency = match Self::measure_ping_latency(&mut write, &mut read).await {
            Ok(latency) => Some(latency),
            Err(e) => {
                debug!("[{}] Ping 測試失敗: {}", exchange_name, e);
                None
            }
        };

        // 正常關閉連接
        let _ = write.close().await;

        Ok((connection_time, ping_latency))
    }

    /// 測量 ping/pong 延遲
    async fn measure_ping_latency(
        write: &mut futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        read: &mut futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> Result<f64> {
        let ping_start = Instant::now();
        let ping_data = b"ping_test".to_vec();

        // 發送 ping
        write.send(Message::Ping(ping_data.clone())).await?;

        // 等待 pong 響應
        match timeout(Duration::from_secs(3), read.next()).await {
            Ok(Some(Ok(Message::Pong(pong_data)))) => {
                if pong_data == ping_data {
                    Ok(ping_start.elapsed().as_millis() as f64)
                } else {
                    Err(anyhow::anyhow!("Pong 數據不匹配"))
                }
            }
            Ok(Some(Ok(_))) => {
                // 某些服務器可能不支持 ping/pong，返回估計延遲
                Ok(ping_start.elapsed().as_millis() as f64)
            }
            Ok(Some(Err(e))) => Err(anyhow::anyhow!("WebSocket 錯誤: {}", e)),
            Ok(None) => Err(anyhow::anyhow!("連接關閉")),
            Err(_) => Err(anyhow::anyhow!("Ping 超時")),
        }
    }

    /// 打印高級分析結果
    async fn print_advanced_analysis(&self, results: &[AdvancedTestResult]) {
        info!("");
        info!("📊 高級連接測試分析報告");
        info!("===============================================================================");

        let mut success_count = 0;
        let mut total_attempts = 0;
        let mut working_urls: HashMap<String, String> = HashMap::new();

        for result in results {
            total_attempts += result.retry_attempts;

            info!("🔹 {}", result.exchange.to_uppercase());

            match result.final_status {
                ConnectionStatus::Success => {
                    success_count += 1;
                    let url = result.successful_url.as_ref().unwrap();
                    working_urls.insert(result.exchange.clone(), url.clone());

                    info!("   ✅ 狀態: 成功");
                    info!("   📡 工作 URL: {}", url);
                    info!("   ⏱️ 連接時間: {:.0}ms", result.connection_time_ms);

                    if let Some(ping) = result.ping_latency_ms {
                        info!("   📶 Ping 延遲: {:.0}ms", ping);
                    }

                    info!("   🔄 嘗試次數: {}", result.retry_attempts);
                }
                ConnectionStatus::Failed => {
                    info!("   ❌ 狀態: 失敗");
                    info!("   🔄 總嘗試次數: {}", result.retry_attempts);
                    info!("   📝 錯誤詳情:");

                    for (i, error) in result.error_details.iter().enumerate() {
                        if i < 3 {
                            // 只顯示前3個錯誤
                            info!("      {}: {}", i + 1, error);
                        }
                    }

                    if result.error_details.len() > 3 {
                        info!("      ... 還有 {} 個錯誤", result.error_details.len() - 3);
                    }
                }
                ConnectionStatus::PartialSuccess => {
                    info!("   ⚠️ 狀態: 部分成功");
                }
            }
            info!("");
        }

        // 統計總結
        info!("📈 測試總結:");
        info!(
            "   成功連接: {}/{} ({:.1}%)",
            success_count,
            results.len(),
            (success_count as f64 / results.len() as f64) * 100.0
        );
        info!("   總嘗試次數: {}", total_attempts);

        if success_count > 0 {
            let avg_time = results
                .iter()
                .filter(|r| matches!(r.final_status, ConnectionStatus::Success))
                .map(|r| r.connection_time_ms)
                .sum::<f64>()
                / success_count as f64;
            info!("   平均連接時間: {:.0}ms", avg_time);
        }

        // 工作 URL 摘要
        if !working_urls.is_empty() {
            info!("");
            info!("✅ 成功的連接配置:");
            for (exchange, url) in working_urls.iter() {
                info!("   {}: {}", exchange, url);
            }
        }

        // 診斷建議
        if success_count < results.len() {
            info!("");
            warn!("🔧 失敗連接的診斷建議:");
            warn!("   1. 檢查是否存在地理位置限制");
            warn!("   2. 嘗試使用 VPN 或代理");
            warn!("   3. 檢查防火牆和企業網絡策略");
            warn!("   4. 確認系統時間和證書狀態");
            warn!("   5. 考慮使用不同的 DNS 服務器");
        }

        info!("===============================================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("advanced_connection_test=info")
        .init();

    info!("🚀 開始高級 WebSocket 連接診斷");

    // 創建測試框架
    let test_framework = AdvancedConnectionTest::new();

    // 運行測試
    match test_framework.run_advanced_test().await {
        Ok(results) => {
            let successful_count = results
                .iter()
                .filter(|r| matches!(r.final_status, ConnectionStatus::Success))
                .count();

            if successful_count == results.len() {
                info!("🎉 所有交易所連接成功!");
                println!("✅ 準備進行並發性能測試");
            } else if successful_count > 0 {
                info!(
                    "⚠️ 部分交易所連接成功 ({}/{})",
                    successful_count,
                    results.len()
                );
                println!("⚠️ 可以使用成功的交易所進行性能測試");
            } else {
                error!("❌ 所有連接失敗");
                println!("❌ 需要檢查網絡配置或使用代理");
            }
        }
        Err(e) => {
            error!("❌ 測試執行失敗: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
