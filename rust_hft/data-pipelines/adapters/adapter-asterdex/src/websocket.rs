//! Aster DEX WebSocket 連接管理（多流端點）

use hft_core::{HftError, HftResult, Symbol};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

const WS_BASE_URL: &str = "wss://fstream.asterdex.com"; // 多流：/stream?streams=

pub struct AsterdexWebSocket;

impl AsterdexWebSocket {
    pub fn new() -> Self {
        Self
    }

    /// 連接並訂閱多個流，使用 /stream?streams=a/b/c 風格
    pub async fn connect_and_subscribe(
        &mut self,
        symbols: Vec<Symbol>,
    ) -> HftResult<WebSocketStream> {
        let streams = Self::build_stream_names(&symbols);
        let stream_param = streams.join("/");
        let url = format!("{}/stream?streams={}", WS_BASE_URL, stream_param);
        let url = Url::parse(&url).map_err(|e| HftError::Parse(format!("URL 解析錯誤: {}", e)))?;

        let (ws_stream, _resp) = connect_async(url)
            .await
            .map_err(|e| HftError::Network(format!("Aster DEX WebSocket 連接失敗: {}", e)))?;

        Ok(WebSocketStream { inner: ws_stream })
    }

    fn build_stream_names(symbols: &[Symbol]) -> Vec<String> {
        // 允許通過環境變數控制深度模式
        // ASTER_USE_DIFF=true -> 使用 diffDepth（symbol@depth@100ms）
        // 否則使用有限檔（symbol@depth{levels}@{freq}），預設 levels=20, freq=100ms
        // 通用命名優先
        let mode = std::env::var("COLLECTOR_DEPTH_MODE")
            .unwrap_or_default()
            .to_lowercase();
        let use_diff_generic = matches!(mode.as_str(), "incremental" | "diff" | "diffdepth");
        let use_diff = use_diff_generic
            || matches!(
                std::env::var("ASTER_USE_DIFF")
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            );
        let levels: usize = std::env::var("COLLECTOR_DEPTH_LEVELS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .or_else(|| {
                std::env::var("ASTER_DEPTH_LEVELS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .unwrap_or(20);
        let freq = std::env::var("COLLECTOR_DEPTH_FREQ")
            .or_else(|_| std::env::var("ASTER_DEPTH_FREQ"))
            .unwrap_or_else(|_| "100ms".to_string());
        let mut streams = Vec::new();
        for symbol in symbols {
            let s = symbol.to_string().to_lowercase();
            if use_diff {
                streams.push(format!("{}@depth@{}", s, freq));
            } else {
                streams.push(format!("{}@depth{}@{}", s, levels, freq));
            }
            streams.push(format!("{}@trade", s));
            // 可選：K 線 1m
            streams.push(format!("{}@kline_1m", s));
        }
        streams
    }
}

pub struct WebSocketStream {
    pub(crate) inner: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl WebSocketStream {
    pub async fn next(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        use futures::StreamExt;
        self.inner.next().await
    }
}
