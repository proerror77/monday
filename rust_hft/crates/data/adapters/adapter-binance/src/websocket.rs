//! Binance WebSocket 連接管理

use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, Symbol};
use tokio::time::{interval, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use url::Url;

const WS_BASE_URL: &str = "wss://stream.binance.com:9443/ws";
const PING_INTERVAL: Duration = Duration::from_secs(20);
const PONG_TIMEOUT: Duration = Duration::from_secs(5);

pub struct BinanceWebSocket {
    url: String,
    symbols: Vec<Symbol>,
    is_connected: bool,
}

impl BinanceWebSocket {
    pub fn new() -> Self {
        Self {
            url: WS_BASE_URL.to_string(),
            symbols: Vec::new(),
            is_connected: false,
        }
    }

    /// 開始連接並訂閱指定品種
    pub async fn connect_and_subscribe(&mut self, symbols: Vec<Symbol>) -> HftResult<WebSocketStream> {
        self.symbols = symbols.clone();
        
        // 構建訂閱流名稱
        let streams = self.build_stream_names(&symbols);
        info!("連接 Binance WebSocket，訂閱流: {:?}", streams);

        // 連接 WebSocket
        let url = if streams.is_empty() {
            // 如果沒有特定流，使用通用端點
            Url::parse(WS_BASE_URL).map_err(|e| HftError::Parse(format!("URL 解析錯誤: {}", e)))?
        } else {
            // 構建多流端點
            let stream_names = streams.join("/");
            let combined_url = format!("{}/{}", WS_BASE_URL, stream_names);
            Url::parse(&combined_url).map_err(|e| HftError::Parse(format!("URL 解析錯誤: {}", e)))?
        };

        let (ws_stream, response) = connect_async(url).await
            .map_err(|e| HftError::Network(format!("Binance WebSocket 連接失敗: {}", e)))?;
        
        info!("Binance WebSocket 連接成功: {:?}", response.status());
        self.is_connected = true;

        Ok(WebSocketStream::new(ws_stream))
    }

    /// 構建訂閱流名稱
    fn build_stream_names(&self, symbols: &[Symbol]) -> Vec<String> {
        let mut streams = Vec::new();
        
        for symbol in symbols {
            let symbol_lower = symbol.to_string().to_lowercase();
            
            // 訂單簿增量更新 (100ms 推送)
            streams.push(format!("{}@depth", symbol_lower));
            
            // 實時交易
            streams.push(format!("{}@trade", symbol_lower));
            
            // 1分鐘K線
            streams.push(format!("{}@kline_1m", symbol_lower));
        }
        
        streams
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected
    }

    pub fn set_disconnected(&mut self) {
        self.is_connected = false;
    }
}

/// WebSocket 流封裝器
pub struct WebSocketStream {
    inner: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

impl WebSocketStream {
    fn new(stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Self {
        Self { inner: stream }
    }

    /// 發送消息
    pub async fn send(&mut self, message: Message) -> HftResult<()> {
        self.inner.send(message).await
            .map_err(|e| HftError::Network(format!("發送消息失敗: {}", e)))
    }

    /// 接收消息
    pub async fn next(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        self.inner.next().await
    }

    /// 發送 ping
    pub async fn send_ping(&mut self) -> HftResult<()> {
        self.send(Message::Ping(vec![])).await
    }

    /// 啟動心跳任務
    pub async fn start_heartbeat(mut self) -> HftResult<()> {
        let mut ping_interval = interval(PING_INTERVAL);
        
        loop {
            tokio::select! {
                _ = ping_interval.tick() => {
                    debug!("發送 ping");
                    if let Err(e) = self.send_ping().await {
                        error!("發送 ping 失敗: {}", e);
                        return Err(e);
                    }
                }
                
                msg = self.next() => {
                    match msg {
                        Some(Ok(Message::Pong(_))) => {
                            debug!("收到 pong");
                        }
                        Some(Ok(Message::Text(text))) => {
                            debug!("收到文本消息: {}", text);
                            // 這裡可以發送到消息處理器
                        }
                        Some(Ok(Message::Close(_))) => {
                            warn!("WebSocket 連接被關閉");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("WebSocket 錯誤: {}", e);
                            return Err(HftError::Network(e.to_string()));
                        }
                        None => {
                            warn!("WebSocket 流結束");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_stream_names() {
        let ws = BinanceWebSocket::new();
        let symbols = vec![
            Symbol::try_from("BTCUSDT").unwrap(),
            Symbol::try_from("ETHUSDT").unwrap(),
        ];
        
        let streams = ws.build_stream_names(&symbols);
        
        assert_eq!(streams.len(), 6); // 每個品種 3 個流
        assert!(streams.contains(&"btcusdt@depth".to_string()));
        assert!(streams.contains(&"btcusdt@trade".to_string()));
        assert!(streams.contains(&"btcusdt@kline_1m".to_string()));
        assert!(streams.contains(&"ethusdt@depth".to_string()));
        assert!(streams.contains(&"ethusdt@trade".to_string()));
        assert!(streams.contains(&"ethusdt@kline_1m".to_string()));
    }
}