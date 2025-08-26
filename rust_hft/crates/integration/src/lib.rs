//! 低階 I/O 積木（不含商業邏輯）
//! - WS/HTTP/TLS
//! - 心跳/重連/簽名/限速 輔助
//! - 僅供 adapters 復用

pub mod ws;
pub mod http;
pub mod signing;
pub mod rate_limit;
pub mod reconnect;

