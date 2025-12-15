//! Redis Pub/Sub 事件通道
//!
//! 支援的通道:
//! - `ml.deploy`: ML 模型部署事件
//! - `ops.alert`: 營運告警事件 (延遲/回撤)
//! - `kill-switch`: 緊急停止信號

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Pub/Sub 錯誤類型
#[derive(Debug, thiserror::Error)]
pub enum PubSubError {
    #[error("Redis 連接錯誤: {0}")]
    Connection(String),
    #[error("發布錯誤: {0}")]
    Publish(String),
    #[error("訂閱錯誤: {0}")]
    Subscribe(String),
    #[error("序列化錯誤: {0}")]
    Serialization(String),
}

impl From<redis::RedisError> for PubSubError {
    fn from(e: redis::RedisError) -> Self {
        PubSubError::Connection(e.to_string())
    }
}

/// Redis 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    /// Redis URL (如 redis://localhost:6379)
    pub url: String,
    /// 連接池大小
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
    /// 連接超時（毫秒）
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
}

fn default_pool_size() -> usize {
    5
}
fn default_connect_timeout_ms() -> u64 {
    3000
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".to_string(),
            pool_size: default_pool_size(),
            connect_timeout_ms: default_connect_timeout_ms(),
        }
    }
}

impl RedisConfig {
    /// 從環境變數建立配置
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            pool_size: std::env::var("REDIS_POOL_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_pool_size()),
            connect_timeout_ms: std::env::var("REDIS_CONNECT_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_connect_timeout_ms()),
        }
    }
}

/// 事件通道定義
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventChannel {
    /// ML 模型部署事件
    MlDeploy,
    /// ML 模型拒絕事件
    MlReject,
    /// 營運告警事件
    OpsAlert,
    /// 緊急停止信號
    KillSwitch,
}

impl EventChannel {
    /// 獲取 Redis 通道名稱
    pub fn channel_name(&self) -> &'static str {
        match self {
            EventChannel::MlDeploy => "ml.deploy",
            EventChannel::MlReject => "ml.reject",
            EventChannel::OpsAlert => "ops.alert",
            EventChannel::KillSwitch => "kill-switch",
        }
    }

    /// 從通道名稱解析
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ml.deploy" => Some(EventChannel::MlDeploy),
            "ml.reject" => Some(EventChannel::MlReject),
            "ops.alert" => Some(EventChannel::OpsAlert),
            "kill-switch" => Some(EventChannel::KillSwitch),
            _ => None,
        }
    }
}

/// ML 部署事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployEvent {
    /// 模型 URL
    pub url: String,
    /// 模型 SHA256 校驗
    pub sha256: String,
    /// 版本標識
    pub version: String,
    /// IC (Information Coefficient)
    pub ic: f64,
    /// IR (Information Ratio)
    pub ir: f64,
    /// 最大回撤
    pub mdd: f64,
    /// 時間戳
    pub ts: i64,
    /// 模型類型
    #[serde(default)]
    pub model_type: String,
}

/// 營運告警事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvent {
    /// 告警類型 (latency, dd, infra, exec_*)
    #[serde(rename = "type")]
    pub alert_type: String,
    /// 當前值
    pub value: f64,
    /// 閾值 (用於延遲告警)
    #[serde(default)]
    pub p99: Option<f64>,
    /// 時間戳
    pub ts: i64,
    /// 交易所 (用於執行告警)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    /// 操作名稱 (用於執行告警)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// 錯誤信息 (用於執行告警)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 失敗計數 (用於執行告警)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_count: Option<u32>,
}

impl AlertEvent {
    /// 創建延遲告警
    pub fn latency(value_ms: f64, p99_ms: f64) -> Self {
        Self {
            alert_type: "latency".to_string(),
            value: value_ms,
            p99: Some(p99_ms),
            ts: chrono::Utc::now().timestamp(),
            venue: None,
            operation: None,
            error: None,
            failure_count: None,
        }
    }

    /// 創建回撤告警
    pub fn drawdown(dd_pct: f64) -> Self {
        Self {
            alert_type: "dd".to_string(),
            value: dd_pct,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: None,
            operation: None,
            error: None,
            failure_count: None,
        }
    }

    /// 創建基礎設施告警
    pub fn infra(value: f64) -> Self {
        Self {
            alert_type: "infra".to_string(),
            value,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: None,
            operation: None,
            error: None,
            failure_count: None,
        }
    }

    /// 創建執行初始化失敗告警
    pub fn exec_init_failed(venue: &str, operation: &str, error: &str) -> Self {
        Self {
            alert_type: "exec_init_failed".to_string(),
            value: 0.0,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: Some(venue.to_string()),
            operation: Some(operation.to_string()),
            error: Some(error.to_string()),
            failure_count: None,
        }
    }

    /// 創建執行連續失敗告警
    pub fn exec_consecutive_failures(venue: &str, operation: &str, count: u32, error: &str) -> Self {
        Self {
            alert_type: "exec_consecutive_failures".to_string(),
            value: count as f64,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: Some(venue.to_string()),
            operation: Some(operation.to_string()),
            error: Some(error.to_string()),
            failure_count: Some(count),
        }
    }

    /// 創建熔斷器開啟告警
    pub fn exec_circuit_open(venue: &str, failure_count: u32, error: &str) -> Self {
        Self {
            alert_type: "exec_circuit_open".to_string(),
            value: failure_count as f64,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: Some(venue.to_string()),
            operation: None,
            error: Some(error.to_string()),
            failure_count: Some(failure_count),
        }
    }

    /// 創建熔斷器恢復告警
    pub fn exec_circuit_recovered(venue: &str) -> Self {
        Self {
            alert_type: "exec_circuit_recovered".to_string(),
            value: 0.0,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: Some(venue.to_string()),
            operation: None,
            error: None,
            failure_count: None,
        }
    }

    /// 創建重試耗盡告警
    pub fn exec_retries_exhausted(venue: &str, operation: &str, max_retries: u32, error: &str) -> Self {
        Self {
            alert_type: "exec_retries_exhausted".to_string(),
            value: max_retries as f64,
            p99: None,
            ts: chrono::Utc::now().timestamp(),
            venue: Some(venue.to_string()),
            operation: Some(operation.to_string()),
            error: Some(error.to_string()),
            failure_count: Some(max_retries),
        }
    }
}

/// Kill-Switch 事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchEvent {
    /// 觸發原因
    pub cause: String,
    /// 時間戳
    pub ts: i64,
}

impl KillSwitchEvent {
    pub fn new(cause: &str) -> Self {
        Self {
            cause: cause.to_string(),
            ts: chrono::Utc::now().timestamp(),
        }
    }
}

/// Redis Pub/Sub 客戶端
pub struct RedisPubSub {
    config: RedisConfig,
    client: redis::Client,
    /// 本地事件廣播器（用於訂閱者接收）
    alert_tx: broadcast::Sender<AlertEvent>,
    deploy_tx: broadcast::Sender<DeployEvent>,
    killswitch_tx: broadcast::Sender<KillSwitchEvent>,
}

impl RedisPubSub {
    /// 建立新的 Pub/Sub 客戶端
    pub fn new(config: RedisConfig) -> Result<Self, PubSubError> {
        let client = redis::Client::open(config.url.clone())?;

        let (alert_tx, _) = broadcast::channel(256);
        let (deploy_tx, _) = broadcast::channel(64);
        let (killswitch_tx, _) = broadcast::channel(16);

        Ok(Self {
            config,
            client,
            alert_tx,
            deploy_tx,
            killswitch_tx,
        })
    }

    /// 建立共享客戶端
    pub fn new_shared(config: RedisConfig) -> Result<Arc<Self>, PubSubError> {
        Ok(Arc::new(Self::new(config)?))
    }

    /// 發布告警事件
    pub async fn publish_alert(&self, event: AlertEvent) -> Result<(), PubSubError> {
        self.publish(EventChannel::OpsAlert, &event).await
    }

    /// 發布部署事件
    pub async fn publish_deploy(&self, event: DeployEvent) -> Result<(), PubSubError> {
        self.publish(EventChannel::MlDeploy, &event).await
    }

    /// 發布 Kill-Switch 事件
    pub async fn publish_killswitch(&self, event: KillSwitchEvent) -> Result<(), PubSubError> {
        self.publish(EventChannel::KillSwitch, &event).await
    }

    /// 通用發布方法
    pub async fn publish<T: Serialize>(
        &self,
        channel: EventChannel,
        event: &T,
    ) -> Result<(), PubSubError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let message = serde_json::to_string(event)
            .map_err(|e| PubSubError::Serialization(e.to_string()))?;

        let _: () = conn
            .publish(channel.channel_name(), &message)
            .await
            .map_err(|e| PubSubError::Publish(e.to_string()))?;

        debug!("發布事件到 {}: {}", channel.channel_name(), message);
        Ok(())
    }

    /// 訂閱告警事件
    pub fn subscribe_alerts(&self) -> broadcast::Receiver<AlertEvent> {
        self.alert_tx.subscribe()
    }

    /// 訂閱部署事件
    pub fn subscribe_deploy(&self) -> broadcast::Receiver<DeployEvent> {
        self.deploy_tx.subscribe()
    }

    /// 訂閱 Kill-Switch 事件
    pub fn subscribe_killswitch(&self) -> broadcast::Receiver<KillSwitchEvent> {
        self.killswitch_tx.subscribe()
    }

    /// 啟動訂閱者背景任務
    pub async fn start_subscriber(self: Arc<Self>) -> Result<tokio::task::JoinHandle<()>, PubSubError> {
        let conn = self.client.get_async_connection().await?;
        let pubsub_conn = conn.into_pubsub();

        let alert_tx = self.alert_tx.clone();
        let deploy_tx = self.deploy_tx.clone();
        let killswitch_tx = self.killswitch_tx.clone();

        let handle = tokio::spawn(async move {
            Self::subscriber_loop(pubsub_conn, alert_tx, deploy_tx, killswitch_tx).await;
        });

        info!("Redis Pub/Sub 訂閱者已啟動");
        Ok(handle)
    }

    /// 訂閱者主循環
    async fn subscriber_loop(
        mut pubsub: redis::aio::PubSub,
        alert_tx: broadcast::Sender<AlertEvent>,
        deploy_tx: broadcast::Sender<DeployEvent>,
        killswitch_tx: broadcast::Sender<KillSwitchEvent>,
    ) {
        use futures_util::StreamExt;

        // 訂閱所有通道
        if let Err(e) = pubsub.subscribe(EventChannel::OpsAlert.channel_name()).await {
            error!("訂閱 ops.alert 失敗: {}", e);
            return;
        }
        if let Err(e) = pubsub.subscribe(EventChannel::MlDeploy.channel_name()).await {
            error!("訂閱 ml.deploy 失敗: {}", e);
            return;
        }
        if let Err(e) = pubsub.subscribe(EventChannel::KillSwitch.channel_name()).await {
            error!("訂閱 kill-switch 失敗: {}", e);
            return;
        }

        info!("已訂閱 ops.alert, ml.deploy, kill-switch 通道");

        let mut stream = pubsub.on_message();

        while let Some(msg) = stream.next().await {
            let channel: String = match msg.get_channel() {
                Ok(c) => c,
                Err(e) => {
                    warn!("無法獲取通道名稱: {}", e);
                    continue;
                }
            };

            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(e) => {
                    warn!("無法獲取消息內容: {}", e);
                    continue;
                }
            };

            debug!("收到消息 [{}]: {}", channel, payload);

            match channel.as_str() {
                "ops.alert" => {
                    match serde_json::from_str::<AlertEvent>(&payload) {
                        Ok(event) => {
                            let _ = alert_tx.send(event);
                        }
                        Err(e) => {
                            warn!("解析 AlertEvent 失敗: {}", e);
                        }
                    }
                }
                "ml.deploy" => {
                    match serde_json::from_str::<DeployEvent>(&payload) {
                        Ok(event) => {
                            let _ = deploy_tx.send(event);
                        }
                        Err(e) => {
                            warn!("解析 DeployEvent 失敗: {}", e);
                        }
                    }
                }
                "kill-switch" => {
                    match serde_json::from_str::<KillSwitchEvent>(&payload) {
                        Ok(event) => {
                            let _ = killswitch_tx.send(event);
                        }
                        Err(e) => {
                            warn!("解析 KillSwitchEvent 失敗: {}", e);
                        }
                    }
                }
                _ => {
                    warn!("未知通道: {}", channel);
                }
            }
        }

        info!("Redis Pub/Sub 訂閱者已停止");
    }

    /// 獲取配置引用
    pub fn config(&self) -> &RedisConfig {
        &self.config
    }

    /// 發送簡單字串消息
    pub async fn publish_raw(&self, channel: &str, message: &str) -> Result<(), PubSubError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .publish(channel, message)
            .await
            .map_err(|e| PubSubError::Publish(e.to_string()))?;

        Ok(())
    }

    /// KV 操作: 設置值
    pub async fn set(&self, key: &str, value: &str) -> Result<(), PubSubError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .set(key, value)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(())
    }

    /// KV 操作: 設置值（帶過期時間）
    pub async fn set_ex(&self, key: &str, value: &str, seconds: u64) -> Result<(), PubSubError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .set_ex(key, value, seconds)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(())
    }

    /// KV 操作: 獲取值
    pub async fn get(&self, key: &str) -> Result<Option<String>, PubSubError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let value: Option<String> = conn
            .get(key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_channel_names() {
        assert_eq!(EventChannel::MlDeploy.channel_name(), "ml.deploy");
        assert_eq!(EventChannel::OpsAlert.channel_name(), "ops.alert");
        assert_eq!(EventChannel::KillSwitch.channel_name(), "kill-switch");
    }

    #[test]
    fn test_event_channel_from_name() {
        assert_eq!(EventChannel::from_name("ml.deploy"), Some(EventChannel::MlDeploy));
        assert_eq!(EventChannel::from_name("ops.alert"), Some(EventChannel::OpsAlert));
        assert_eq!(EventChannel::from_name("unknown"), None);
    }

    #[test]
    fn test_alert_event_serialization() {
        let event = AlertEvent::latency(25.5, 15.0);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"latency\""));
        assert!(json.contains("\"value\":25.5"));
        assert!(json.contains("\"p99\":15.0"));
    }

    #[test]
    fn test_deploy_event_serialization() {
        let event = DeployEvent {
            url: "supabase://models/test.pt".to_string(),
            sha256: "abc123".to_string(),
            version: "v1.0".to_string(),
            ic: 0.031,
            ir: 1.28,
            mdd: 0.041,
            ts: 1721904000,
            model_type: "sup".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"ic\":0.031"));
        assert!(json.contains("\"ir\":1.28"));
    }

    #[test]
    fn test_killswitch_event() {
        let event = KillSwitchEvent::new("DD exceeded 5%");
        assert_eq!(event.cause, "DD exceeded 5%");
        assert!(event.ts > 0);
    }

    #[test]
    fn test_config_default() {
        let config = RedisConfig::default();
        assert_eq!(config.url, "redis://localhost:6379");
        assert_eq!(config.pool_size, 5);
    }

    #[test]
    fn test_alert_event_types() {
        let latency = AlertEvent::latency(25.0, 15.0);
        assert_eq!(latency.alert_type, "latency");
        assert!(latency.p99.is_some());

        let dd = AlertEvent::drawdown(3.5);
        assert_eq!(dd.alert_type, "dd");
        assert!(dd.p99.is_none());

        let infra = AlertEvent::infra(5.0);
        assert_eq!(infra.alert_type, "infra");
    }
}
