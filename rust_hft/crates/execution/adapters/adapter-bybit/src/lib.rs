//! Bybit 执行适配器 - 占位符实现
//!
//! 提供对 Bybit v5 REST API 和私有 WebSocket 的基础接口

use tracing::warn;
use hft_core::HftError;
use integration::signing::BybitCredentials;

/// Bybit 执行模式
#[derive(Debug, Clone)]
pub enum ExecutionMode {
    Live,
    Paper, 
    Testnet,
}

/// Bybit 执行客户端配置
#[derive(Debug, Clone)]
pub struct BybitExecutionConfig {
    pub credentials: BybitCredentials,
    pub mode: ExecutionMode,
    pub rest_base_url: String,
    pub ws_private_url: String,
    pub timeout_ms: u64,
}

/// Bybit 执行客户端占位符
pub struct BybitExecutionClient {
    config: BybitExecutionConfig,
}

impl BybitExecutionClient {
    pub fn new(config: BybitExecutionConfig) -> Result<Self, HftError> {
        warn!("Bybit 执行客户端为占位符实现");
        Ok(Self { config })
    }
}

// 不实现 ExecutionClient trait，仅提供基础结构供 runtime feature 编译通过
