//! 多交易所 API 签名实现
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct BitgetCredentials {
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
}

impl BitgetCredentials {
    pub fn new(api_key: String, secret_key: String, passphrase: String) -> Self {
        Self { api_key, secret_key, passphrase }
    }
}

/// Bitget API 簽名生成器
pub struct BitgetSigner {
    credentials: BitgetCredentials,
}

impl BitgetSigner {
    pub fn new(credentials: BitgetCredentials) -> Self {
        Self { credentials }
    }

    /// 生成當前時間戳（毫秒）
    pub fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64
    }

    /// 生成簽名標頭
    pub fn generate_headers(
        &self,
        method: &str,
        request_path: &str,
        body: &str,
        timestamp: Option<u64>,
    ) -> HashMap<String, String> {
        let timestamp = timestamp.unwrap_or_else(Self::current_timestamp);
        
        // 生成簽名字符串：timestamp + method + requestPath + body
        let sign_string = format!("{}{}{}{}", timestamp, method, request_path, body);
        
        // HMAC-SHA256 簽名
        let signature = self.sign_hmac_sha256(&sign_string);
        
        let mut headers = HashMap::new();
        headers.insert("ACCESS-KEY".to_string(), self.credentials.api_key.clone());
        headers.insert("ACCESS-SIGN".to_string(), signature);
        headers.insert("ACCESS-TIMESTAMP".to_string(), timestamp.to_string());
        headers.insert("ACCESS-PASSPHRASE".to_string(), self.credentials.passphrase.clone());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        
        headers
    }

    /// HMAC-SHA256 簽名
    fn sign_hmac_sha256(&self, message: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.credentials.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        
        mac.update(message.as_bytes());
        let result = mac.finalize();
        
        // 返回 base64 編碼的簽名
        use base64::prelude::*;
        BASE64_STANDARD.encode(result.into_bytes())
    }
}

// 向後兼容的通用簽名函數
pub fn sign_request(secret: &str, payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    
    mac.update(payload.as_bytes());
    let result = mac.finalize();
    
    use base64::prelude::*;
    BASE64_STANDARD.encode(result.into_bytes())
}

///// BINANCE API 签名 /////

#[derive(Clone, Debug)]
pub struct BinanceCredentials {
    pub api_key: String,
    pub secret_key: String,
}

impl BinanceCredentials {
    pub fn new(api_key: String, secret_key: String) -> Self {
        Self { api_key, secret_key }
    }
}

/// Binance API 签名生成器
pub struct BinanceSigner {
    credentials: BinanceCredentials,
}

impl BinanceSigner {
    pub fn new(credentials: BinanceCredentials) -> Self {
        Self { credentials }
    }
    
    /// 生成当前时间戳（毫秒）
    pub fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64
    }

    /// 生成签名（用于查询参数）
    pub fn sign_query(&self, query: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.credentials.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        
        mac.update(query.as_bytes());
        let result = mac.finalize();
        
        hex::encode(result.into_bytes())
    }

    /// 生成带签名的查询参数
    pub fn sign_request(&self, params: &mut HashMap<String, String>) -> String {
        // 添加时间戳
        params.insert("timestamp".to_string(), Self::current_timestamp().to_string());
        
        // 构建查询字符串
        let mut query_pairs: Vec<_> = params.iter().collect();
        query_pairs.sort_by_key(|&(key, _)| key);
        let query = query_pairs
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
        
        // 生成签名
        let signature = self.sign_query(&query);
        
        format!("{}&signature={}", query, signature)
    }

    /// 生成请求头
    pub fn generate_headers(&self) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("X-MBX-APIKEY".to_string(), self.credentials.api_key.clone());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers
    }
}

///// BYBIT API 签名 /////

#[derive(Clone, Debug)]
pub struct BybitCredentials {
    pub api_key: String,
    pub secret_key: String,
}

impl BybitCredentials {
    pub fn new(api_key: String, secret_key: String) -> Self {
        Self { api_key, secret_key }
    }
}

/// Bybit API 签名生成器
pub struct BybitSigner {
    credentials: BybitCredentials,
}

impl BybitSigner {
    pub fn new(credentials: BybitCredentials) -> Self {
        Self { credentials }
    }
    
    /// 生成当前时间戳（毫秒）
    pub fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64
    }

    /// 生成签名 (v5 API)
    pub fn generate_headers(
        &self,
        _method: &str,
        _request_path: &str,
        params: &str,
        timestamp: Option<u64>,
    ) -> HashMap<String, String> {
        let timestamp = timestamp.unwrap_or_else(Self::current_timestamp);
        let recv_window = "5000"; // 5s 接收窗口
        
        // 生成签名字符串：timestamp + apiKey + recvWindow + params
        let sign_string = format!("{}{}{}{}", timestamp, self.credentials.api_key, recv_window, params);
        
        // HMAC-SHA256 签名
        let signature = self.sign_hmac_sha256(&sign_string);
        
        let mut headers = HashMap::new();
        headers.insert("X-BAPI-API-KEY".to_string(), self.credentials.api_key.clone());
        headers.insert("X-BAPI-SIGN".to_string(), signature);
        headers.insert("X-BAPI-TIMESTAMP".to_string(), timestamp.to_string());
        headers.insert("X-BAPI-RECV-WINDOW".to_string(), recv_window.to_string());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        
        headers
    }

    /// HMAC-SHA256 签名
    fn sign_hmac_sha256(&self, message: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.credentials.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        
        mac.update(message.as_bytes());
        let result = mac.finalize();
        
        hex::encode(result.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitget_signing() {
        let credentials = BitgetCredentials::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_passphrase".to_string(),
        );
        
        let signer = BitgetSigner::new(credentials);
        let headers = signer.generate_headers("GET", "/api/v2/spot/account/assets", "", Some(1640995200000));
        
        assert!(headers.contains_key("ACCESS-KEY"));
        assert!(headers.contains_key("ACCESS-SIGN"));
        assert!(headers.contains_key("ACCESS-TIMESTAMP"));
        assert!(headers.contains_key("ACCESS-PASSPHRASE"));
    }

    #[test]
    fn test_binance_signing() {
        let credentials = BinanceCredentials::new(
            "test_key".to_string(),
            "test_secret".to_string(),
        );
        
        let signer = BinanceSigner::new(credentials);
        let mut params = HashMap::new();
        params.insert("symbol".to_string(), "BTCUSDT".to_string());
        
        let query = signer.sign_request(&mut params);
        assert!(query.contains("signature="));
        assert!(query.contains("timestamp="));
    }

    #[test]
    fn test_bybit_signing() {
        let credentials = BybitCredentials::new(
            "test_key".to_string(),
            "test_secret".to_string(),
        );
        
        let signer = BybitSigner::new(credentials);
        let headers = signer.generate_headers("GET", "/v5/account/wallet-balance", "", Some(1640995200000));
        
        assert!(headers.contains_key("X-BAPI-API-KEY"));
        assert!(headers.contains_key("X-BAPI-SIGN"));
        assert!(headers.contains_key("X-BAPI-TIMESTAMP"));
        assert!(headers.contains_key("X-BAPI-RECV-WINDOW"));
    }
}
