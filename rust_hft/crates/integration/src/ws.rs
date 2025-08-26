//! WebSocket 基元 - 高性能 WebSocket 客戶端
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;
use futures_util::{SinkExt, StreamExt};
use tracing::{trace, warn, error, info};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub struct WsClientConfig {
    pub url: String,
    pub heartbeat_interval: Duration,
    pub reconnect_interval: Duration,
    pub max_reconnect_attempts: u32,
}

impl Default for WsClientConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            heartbeat_interval: Duration::from_secs(30),
            reconnect_interval: Duration::from_secs(5),
            max_reconnect_attempts: 10,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WsMetrics {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub reconnect_count: u32,
    pub last_heartbeat: Option<Instant>,
}

pub type WsConnection = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
pub struct WsClient {
    pub cfg: WsClientConfig,
    pub metrics: WsMetrics,
    connection: Option<WsConnection>,
}

impl WsClient {
    pub fn new(cfg: WsClientConfig) -> Self {
        Self {
            cfg,
            metrics: WsMetrics::default(),
            connection: None,
        }
    }
    
    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("連接到 WebSocket: {}", self.cfg.url);
        
        match connect_async(&self.cfg.url).await {
            Ok((ws_stream, response)) => {
                info!("WebSocket 連接成功，狀態碼: {}", response.status());
                self.connection = Some(ws_stream);
                Ok(())
            }
            Err(e) => {
                error!("WebSocket 連接失敗: {}", e);
                Err(Box::new(e))
            }
        }
    }
    
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(mut conn) = self.connection.take() {
            trace!("正在關閉 WebSocket 連接");
            let _ = conn.close(None).await;
        }
        Ok(())
    }
    
    pub async fn send_message(&mut self, message: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(conn) = &mut self.connection {
            conn.send(Message::Text(message.to_string())).await?;
            self.metrics.messages_sent += 1;
            trace!("發送消息: {}", message);
            Ok(())
        } else {
            Err("WebSocket 未連接".into())
        }
    }
    
    pub async fn receive_message(&mut self) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(conn) = &mut self.connection {
            if let Some(msg) = conn.next().await {
                match msg? {
                    Message::Text(text) => {
                        self.metrics.messages_received += 1;
                        trace!("接收到消息: {}", text);
                        Ok(Some(text))
                    }
                    Message::Ping(payload) => {
                        // 自動回應 Ping
                        conn.send(Message::Pong(payload)).await?;
                        self.metrics.last_heartbeat = Some(Instant::now());
                        Ok(None)
                    }
                    Message::Pong(_) => {
                        self.metrics.last_heartbeat = Some(Instant::now());
                        Ok(None)
                    }
                    Message::Close(_) => {
                        warn!("WebSocket 連接被遠程關閉");
                        self.connection = None;
                        Ok(None)
                    }
                    Message::Binary(data) => {
                        // 對於二進制數據，轉換為 UTF-8
                        match String::from_utf8(data) {
                            Ok(text) => {
                                self.metrics.messages_received += 1;
                                Ok(Some(text))
                            }
                            Err(_) => {
                                warn!("收到無法解析的二進制數據");
                                Ok(None)
                            }
                        }
                    }
                    Message::Frame(_) => Ok(None),
                }
            } else {
                // 連接已斷開
                self.connection = None;
                Ok(None)
            }
        } else {
            Err("WebSocket 未連接".into())
        }
    }
    
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }
    
    pub async fn send_ping(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(conn) = &mut self.connection {
            conn.send(Message::Ping(vec![])).await?;
            trace!("發送心跳 ping");
            Ok(())
        } else {
            Err("WebSocket 未連接".into())
        }
    }
}

/// WebSocket 消息處理器特質
pub trait MessageHandler: Send + Sync {
    fn handle_message(&mut self, message: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn handle_disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    
    /// 連接建立後調用，可用於發送訂閱消息
    fn handle_connected(&mut self, client: &mut WsClient) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _ = client; // 默認實現不做任何事
        Ok(())
    }
    
    /// 連接建立後的異步初始化，可用於發送訂閱消息
    fn handle_connected_async<'a>(&'a mut self, client: &'a mut WsClient) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        Box::pin(async move {
            // 默認調用同步版本
            self.handle_connected(client)
        })
    }
}

/// 帶重連的 WebSocket 客戶端
pub struct ReconnectingWsClient {
    pub client: WsClient,
    reconnect_attempts: u32,
}

impl ReconnectingWsClient {
    pub fn new(config: WsClientConfig) -> Self {
        Self {
            client: WsClient::new(config),
            reconnect_attempts: 0,
        }
    }
    
    pub async fn connect_with_retry(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        loop {
            match self.client.connect().await {
                Ok(()) => {
                    self.reconnect_attempts = 0;
                    return Ok(());
                }
                Err(e) => {
                    self.reconnect_attempts += 1;
                    self.client.metrics.reconnect_count += 1;
                    
                    if self.reconnect_attempts >= self.client.cfg.max_reconnect_attempts {
                        error!("達到最大重連次數 ({}), 放棄連接", self.client.cfg.max_reconnect_attempts);
                        return Err(e);
                    }
                    
                    warn!("連接失敗，{}秒後重試 (第 {} 次)", 
                          self.client.cfg.reconnect_interval.as_secs(),
                          self.reconnect_attempts);
                    
                    tokio::time::sleep(self.client.cfg.reconnect_interval).await;
                }
            }
        }
    }
    
    pub async fn run_with_handler<H>(&mut self, mut handler: H) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        H: MessageHandler + 'static,
    {
        self.connect_with_retry().await?;
        
        // 連接成功後調用處理器的異步初始化方法
        if let Err(e) = handler.handle_connected_async(&mut self.client).await {
            error!("處理器初始化失敗: {}", e);
            return Err(e);
        }
        
        // 創建心跳任務
        let (heartbeat_tx, mut heartbeat_rx) = mpsc::channel(1);
        let heartbeat_interval = self.client.cfg.heartbeat_interval;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(heartbeat_interval);
            loop {
                interval.tick().await;
                if heartbeat_tx.send(()).await.is_err() {
                    break;
                }
            }
        });
        
        loop {
            tokio::select! {
                // 處理心跳
                _ = heartbeat_rx.recv() => {
                    if self.client.is_connected() {
                        if let Err(e) = self.client.send_ping().await {
                            warn!("發送心跳失敗: {}", e);
                            if let Err(e) = handler.handle_disconnect() {
                                error!("處理斷線事件失敗: {}", e);
                            }
                            self.connect_with_retry().await?;
                            if let Err(e) = handler.handle_connected_async(&mut self.client).await {
                                error!("重連後處理器初始化失敗: {}", e);
                                return Err(e);
                            }
                        }
                    }
                }
                
                // 處理消息
                msg_result = self.client.receive_message() => {
                    match msg_result {
                        Ok(Some(message)) => {
                            if let Err(e) = handler.handle_message(message) {
                                error!("處理消息失敗: {}", e);
                            }
                        }
                        Ok(None) => {
                            // 連接斷開或收到非文本消息
                            if !self.client.is_connected() {
                                warn!("WebSocket 連接已斷開，嘗試重連");
                                if let Err(e) = handler.handle_disconnect() {
                                    error!("處理斷線事件失敗: {}", e);
                                }
                                self.connect_with_retry().await?;
                                if let Err(e) = handler.handle_connected_async(&mut self.client).await {
                                    error!("重連後處理器初始化失敗: {}", e);
                                    return Err(e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("接收消息失敗: {}", e);
                            if let Err(e) = handler.handle_disconnect() {
                                error!("處理斷線事件失敗: {}", e);
                            }
                            self.connect_with_retry().await?;
                            if let Err(e) = handler.handle_connected_async(&mut self.client).await {
                                error!("重連後處理器初始化失敗: {}", e);
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }
    }
}

