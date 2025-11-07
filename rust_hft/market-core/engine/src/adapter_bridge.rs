//! Adapter 橋接層
//!
//! 將 MarketStream 適配器連接到 Engine 的 SPSC ring buffer，
//! 實現有界匯流與背壓控制

use crate::dataflow::{EventConsumer, EventIngester, IngestionConfig};
use futures::StreamExt;
use hft_core::{HftError, Symbol};
use ports::MarketStream;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{debug, info, trace, warn};

/// Adapter 橋接器配置
#[derive(Debug, Clone)]
pub struct AdapterBridgeConfig {
    pub ingestion: IngestionConfig,
    pub max_concurrent_adapters: usize,
}

impl Default for AdapterBridgeConfig {
    fn default() -> Self {
        Self {
            ingestion: IngestionConfig::default(),
            max_concurrent_adapters: 8,
        }
    }
}

/// Adapter 橋接器
/// 負責將多個 MarketStream 橋接到 Engine 的事件消費管道
pub struct AdapterBridge {
    config: AdapterBridgeConfig,
    shutdown_notify: Arc<Notify>,
    is_running: bool,
    /// 可選：引擎喚醒通知器，用於入隊後立即喚醒引擎
    engine_notify: Option<Arc<Notify>>,
}

impl AdapterBridge {
    pub fn new(config: AdapterBridgeConfig) -> Self {
        Self {
            config,
            shutdown_notify: Arc::new(Notify::new()),
            is_running: false,
            engine_notify: None,
        }
    }

    /// 設置引擎喚醒通知器，使攝取成功入隊時能立即喚醒引擎
    pub fn set_engine_notify(&mut self, notify: Arc<Notify>) {
        self.engine_notify = Some(notify);
    }

    /// 橋接單個 MarketStream 到 Engine
    /// 返回 EventConsumer 供 Engine 使用
    pub async fn bridge_stream<S>(
        &mut self,
        stream: S,
        symbols: Vec<Symbol>,
    ) -> Result<EventConsumer, HftError>
    where
        S: MarketStream + Send + 'static,
    {
        info!("橋接市場數據流，交易對: {:?}", symbols);

        // 創建 SPSC 攝取器
        let (mut ingester, consumer) = EventIngester::new(self.config.ingestion.clone());

        // 若有引擎 Notify，掛載到 ingester 以實現真正事件驅動
        if let Some(n) = &self.engine_notify {
            ingester.set_engine_notify(n.clone());
        }

        // 訂閱數據流
        let event_stream = stream
            .subscribe(symbols)
            .await
            .map_err(|e| HftError::Generic {
                message: format!("訂閱失敗: {}", e),
            })?;

        // 在後台任務中運行攝取
        let shutdown_listener = self.shutdown_notify.clone();
        tokio::spawn(async move {
            let mut event_stream = event_stream;
            let mut events_processed = 0u64;

            loop {
                tokio::select! {
                    // 監聽關閉信號
                    _ = shutdown_listener.notified() => {
                        info!("收到關閉信號，停止事件攝取");
                        break;
                    }

                    // 處理事件流
                    event_result = event_stream.next() => {
                        match event_result {
                            Some(Ok(event)) => {
                                events_processed += 1;

                                // 🔥 降低日誌等級：事件級記錄改為 debug!（避免洪流）
                                match &event {
                                    ports::MarketEvent::Bar(bar) => {
                                        trace!(
                                            seq = events_processed,
                                            event_kind = "bar",
                                            symbol = %bar.symbol,
                                            close = %bar.close,
                                            source_venue = ?bar.source_venue,
                                            "AdapterBridge 收到事件"
                                        );
                                    }
                                    ports::MarketEvent::Trade(trade) => {
                                        trace!(
                                            seq = events_processed,
                                            event_kind = "trade",
                                            symbol = %trade.symbol,
                                            price = %trade.price,
                                            qty = %trade.quantity,
                                            source_venue = ?trade.source_venue,
                                            "AdapterBridge 收到事件"
                                        );
                                    }
                                    ports::MarketEvent::Snapshot(snap) => {
                                        trace!(
                                            seq = events_processed,
                                            event_kind = "snapshot",
                                            symbol = %snap.symbol,
                                            sequence = snap.sequence,
                                            source_venue = ?snap.source_venue,
                                            "AdapterBridge 收到事件"
                                        );
                                    }
                                    _ => {
                                        trace!(
                                            seq = events_processed,
                                            event_kind = "other",
                                            "AdapterBridge 收到事件"
                                        );
                                    }
                                }

                                if let Err(e) = ingester.ingest(event) {
                                    tracing::error!("攝取事件失敗: {}", e);
                                    // 繼續處理，不中斷流
                                } else {
                                    trace!("事件成功攝取到 ring buffer");
                                }

                                // 統計輸出降為 debug 並降低頻率
                                if events_processed.is_multiple_of(2000) {
                                    let metrics = ingester.metrics();
                                    debug!("攝取統計: 接收 {}, 丟棄 {}, 陳舊 {}, 最大利用率 {:.2}%",
                                           metrics.events_received,
                                           metrics.events_dropped,
                                           metrics.events_stale,
                                           metrics.ring_utilization_max * 100.0);
                                }
                            }
                            Some(Err(e)) => {
                                warn!("事件流錯誤: {}", e);
                                // 短暫延遲後繼續
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                            None => {
                                warn!("事件流結束");
                                break;
                            }
                        }
                    }
                }
            }

            // 清理
            let final_metrics = ingester.metrics();
            info!(
                "攝取器關閉，最終統計: 接收 {}, 丟棄 {}, 陳舊 {}",
                final_metrics.events_received,
                final_metrics.events_dropped,
                final_metrics.events_stale
            );
        });

        self.is_running = true;
        Ok(consumer)
    }

    /// 橋接多個 MarketStream 到 Engine
    /// 每個 stream 會創建獨立的 SPSC ring buffer
    pub async fn bridge_multiple_streams<S>(
        &mut self,
        streams_and_symbols: Vec<(S, Vec<Symbol>)>,
    ) -> Result<Vec<EventConsumer>, HftError>
    where
        S: MarketStream + Send + 'static,
    {
        if streams_and_symbols.len() > self.config.max_concurrent_adapters {
            return Err(HftError::Generic {
                message: format!(
                    "超過最大並發適配器數量: {} > {}",
                    streams_and_symbols.len(),
                    self.config.max_concurrent_adapters
                ),
            });
        }

        let mut consumers = Vec::new();

        for (stream, symbols) in streams_and_symbols {
            let consumer = self.bridge_stream(stream, symbols).await?;
            consumers.push(consumer);
        }

        self.is_running = true;
        info!("成功橋接 {} 個市場數據流", consumers.len());

        Ok(consumers)
    }

    /// 停止所有橋接器
    pub async fn shutdown(&mut self) {
        if self.is_running {
            info!("正在關閉 Adapter 橋接器");
            self.shutdown_notify.notify_waiters();
            self.is_running = false;

            // 給後台任務一些時間優雅關閉
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            info!("Adapter 橋接器已關閉");
        }
    }

    /// 檢查橋接器狀態
    pub fn is_running(&self) -> bool {
        self.is_running
    }
}

impl Drop for AdapterBridge {
    fn drop(&mut self) {
        if self.is_running {
            // 發送關閉信號
            self.shutdown_notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataflow::FlipPolicy;

    #[test]
    fn test_adapter_bridge_config() {
        let config = AdapterBridgeConfig::default();
        assert_eq!(config.max_concurrent_adapters, 8);
        assert!(matches!(config.ingestion.flip_policy, FlipPolicy::OnUpdate));
    }

    #[tokio::test]
    async fn test_adapter_bridge_creation() {
        let config = AdapterBridgeConfig::default();
        let mut bridge = AdapterBridge::new(config);
        assert!(!bridge.is_running());
    }
}
