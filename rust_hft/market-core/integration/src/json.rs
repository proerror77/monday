//! Feature-gated JSON 解析模塊
//!
//! 提供統一的 JSON 解析接口，根據 feature flag 選擇使用：
//! - `serde_json` (default): 穩定、成熟、兼容性好
//! - `simd-json` (feature = "json-simd"): SIMD 加速，性能優先
//!
//! # Feature Flags
//! - `json-simd`: 啟用 SIMD JSON 解析 (預期 2-4x 性能提升)
//!
//! # 使用範例
//! ```no_run
//! use integration::json::{parse_json_bytes, parse_json_str};
//! use bytes::Bytes;
//!
//! // 從 Bytes 解析 (零拷貝路徑)
//! let bytes = Bytes::from(r#"{"price": 50000.0}"#);
//! let mut buf = bytes.to_vec();
//! let value = parse_json_bytes(&mut buf)?;
//!
//! // 從 &str 解析 (便捷路徑)
//! let text = r#"{"price": 50000.0}"#;
//! let value = parse_json_str(text)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::error::Error as StdError;
use std::fmt;

// ============================================================================
// 類型定義：根據 feature flag 選擇 JSON 值類型
// ============================================================================

/// 統一的 JSON 值類型
///
/// - 當 `json-simd` feature 未啟用時：使用 `serde_json::Value`
/// - 當 `json-simd` feature 啟用時：使用 `simd_json::OwnedValue`
#[cfg(not(feature = "json-simd"))]
pub use serde_json::Value;

#[cfg(feature = "json-simd")]
pub use simd_json::OwnedValue as Value;

// ============================================================================
// 錯誤類型
// ============================================================================

/// JSON 解析錯誤
#[derive(Debug)]
pub struct JsonError {
    message: String,
}

impl JsonError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON 解析錯誤: {}", self.message)
    }
}

impl StdError for JsonError {}

#[cfg(not(feature = "json-simd"))]
impl From<serde_json::Error> for JsonError {
    fn from(err: serde_json::Error) -> Self {
        JsonError::new(err.to_string())
    }
}

#[cfg(feature = "json-simd")]
impl From<simd_json::Error> for JsonError {
    fn from(err: simd_json::Error) -> Self {
        JsonError::new(err.to_string())
    }
}

// ============================================================================
// 解析函數：serde_json 實現 (default)
// ============================================================================

#[cfg(not(feature = "json-simd"))]
pub fn parse_json_bytes(bytes: &mut [u8]) -> Result<Value, JsonError> {
    // serde_json 需要有效的 UTF-8
    let text =
        std::str::from_utf8(bytes).map_err(|e| JsonError::new(format!("UTF-8 驗證失敗: {}", e)))?;
    serde_json::from_str(text).map_err(Into::into)
}

#[cfg(not(feature = "json-simd"))]
pub fn parse_json_str(s: &str) -> Result<Value, JsonError> {
    serde_json::from_str(s).map_err(Into::into)
}

// ============================================================================
// 解析函數：simd-json 實現 (feature = "json-simd")
// ============================================================================

#[cfg(feature = "json-simd")]
pub fn parse_json_bytes(bytes: &mut [u8]) -> Result<Value, JsonError> {
    // simd-json 直接在可變緩衝區上操作，真正的零拷貝
    simd_json::to_owned_value(bytes).map_err(Into::into)
}

#[cfg(feature = "json-simd")]
pub fn parse_json_str(s: &str) -> Result<Value, JsonError> {
    // simd-json 需要可變緩衝區，這裡需要複製一次
    let mut bytes = s.as_bytes().to_vec();
    simd_json::to_owned_value(&mut bytes).map_err(Into::into)
}

// ============================================================================
// 便捷方法：從 Bytes 解析
// ============================================================================

/// 從 `bytes::Bytes` 解析 JSON
///
/// 這是零拷貝 WebSocket 接口的推薦使用方式。
pub fn parse_json_from_bytes(bytes: bytes::Bytes) -> Result<Value, JsonError> {
    let mut buf = bytes.to_vec();
    parse_json_bytes(&mut buf)
}

// ============================================================================
// 測試
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_str() {
        let json = r#"{"price": 50000.0, "symbol": "BTCUSDT"}"#;
        let value = parse_json_str(json).unwrap();

        #[cfg(not(feature = "json-simd"))]
        {
            assert_eq!(value["price"], 50000.0);
            assert_eq!(value["symbol"], "BTCUSDT");
        }

        #[cfg(feature = "json-simd")]
        {
            use simd_json::ValueAccess;
            assert_eq!(value["price"].as_f64(), Some(50000.0));
            assert_eq!(value["symbol"].as_str(), Some("BTCUSDT"));
        }
    }

    #[test]
    fn test_parse_json_bytes() {
        let json = r#"{"price": 50000.0, "symbol": "BTCUSDT"}"#;
        let mut bytes = json.as_bytes().to_vec();
        let value = parse_json_bytes(&mut bytes).unwrap();

        #[cfg(not(feature = "json-simd"))]
        {
            assert_eq!(value["price"], 50000.0);
            assert_eq!(value["symbol"], "BTCUSDT");
        }

        #[cfg(feature = "json-simd")]
        {
            use simd_json::ValueAccess;
            assert_eq!(value["price"].as_f64(), Some(50000.0));
            assert_eq!(value["symbol"].as_str(), Some("BTCUSDT"));
        }
    }

    #[test]
    fn test_parse_invalid_json() {
        let invalid = r#"{"price": 50000.0"#; // 缺少結束括號
        let result = parse_json_str(invalid);
        assert!(result.is_err());
    }
}
