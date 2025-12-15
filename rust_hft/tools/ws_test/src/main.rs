//! WebSocket 連線測試工具
//!
//! 測試所有交易所的 WebSocket 即時連線狀況
//!
//! 支持的交易所：
//! - Bitget
//! - Binance
//! - Bybit
//! - Hyperliquid
//! - Backpack
//! - Asterdex
//! - GRVT
//! - Lighter

use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::main]
async fn main() {
    println!("\n");
    println!("╔═══════════════════════════════════════════════════════════════════╗");
    println!("║          WebSocket 連線測試 - HFT 交易所接入測試                  ║");
    println!("║                     測試所有 8 個交易所                           ║");
    println!("╚═══════════════════════════════════════════════════════════════════╝");
    println!();

    // 測試所有交易所
    let results = vec![
        ("Bitget", test_bitget().await),
        ("Binance", test_binance().await),
        ("Bybit", test_bybit().await),
        ("Hyperliquid", test_hyperliquid().await),
        ("Backpack", test_backpack().await),
        ("Asterdex", test_asterdex().await),
        ("GRVT", test_grvt().await),
        ("Lighter", test_lighter().await),
    ];

    // 打印總結
    println!("\n");
    println!("╔═══════════════════════════════════════════════════════════════════╗");
    println!("║                         測試總結                                  ║");
    println!("╠═══════════════════════════════════════════════════════════════════╣");

    for (name, result) in &results {
        let status = if result.success { "✓ 成功" } else { "✗ 失敗" };
        let latency = if result.connect_latency_ms > 0 {
            format!("{:>6}ms", result.connect_latency_ms)
        } else {
            "   N/A".to_string()
        };
        let msgs = if result.messages_received > 0 {
            format!("{:>3} 條", result.messages_received)
        } else {
            "  0 條".to_string()
        };

        println!("║ {:12} │ {:8} │ 延遲: {} │ 消息: {} ║",
            name, status, latency, msgs);
    }

    println!("╚═══════════════════════════════════════════════════════════════════╝");

    let success_count = results.iter().filter(|(_, r)| r.success).count();
    let total = results.len();
    println!("\n  總計: {}/{} 交易所連線成功\n", success_count, total);
}

#[derive(Debug, Clone)]
struct TestResult {
    success: bool,
    connect_latency_ms: u64,
    messages_received: usize,
    error: Option<String>,
}

impl Default for TestResult {
    fn default() -> Self {
        Self {
            success: false,
            connect_latency_ms: 0,
            messages_received: 0,
            error: None,
        }
    }
}

async fn test_bitget() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [1/8] Bitget                                                │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://ws.bitget.com/v2/ws/public";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    let subscribe_msg = serde_json::json!({
        "op": "subscribe",
        "args": [{"instType": "SPOT", "channel": "books15", "instId": "BTCUSDT"}]
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_binance() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [2/8] Binance                                               │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://stream.binance.com:9443/ws/btcusdt@depth20@100ms";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    let _ = timeout(Duration::from_secs(3), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_bybit() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [3/8] Bybit                                                 │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://stream.bybit.com/v5/public/spot";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    let subscribe_msg = serde_json::json!({
        "op": "subscribe",
        "args": ["orderbook.50.BTCUSDT"]
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_hyperliquid() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [4/8] Hyperliquid                                           │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://api.hyperliquid.xyz/ws";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    // Hyperliquid 使用 JSON-RPC 訂閱
    let subscribe_msg = serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "l2Book", "coin": "BTC"}
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_backpack() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [5/8] Backpack                                              │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://ws.backpack.exchange";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    // Backpack 訂閱格式
    let subscribe_msg = serde_json::json!({
        "method": "SUBSCRIBE",
        "params": ["depth.SOL_USDC"]
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_asterdex() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [6/8] Asterdex                                              │");
    println!("└─────────────────────────────────────────────────────────────┘");

    // Asterdex 使用 URL 多流訂閱
    let url = "wss://fstream.asterdex.com/stream?streams=btcusdt@depth20@100ms";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 5 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_grvt() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [7/8] GRVT (Testnet)                                        │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://trades.testnet.grvt.io/ws";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    // GRVT 使用 JSON-RPC 訂閱
    let subscribe_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "subscribe",
        "params": {
            "stream": "v1.book.s",
            "selectors": ["BTC_USDT_Perp"]
        },
        "id": 1
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 3 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}

async fn test_lighter() -> TestResult {
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [8/8] Lighter                                               │");
    println!("└─────────────────────────────────────────────────────────────┘");

    let url = "wss://mainnet.zklighter.elliot.ai/stream";
    let start = Instant::now();
    let mut result = TestResult::default();

    println!("  URL: {}", url);

    let connect_result = timeout(Duration::from_secs(10), connect_async(url)).await;

    let (mut ws_stream, _) = match connect_result {
        Ok(Ok((stream, resp))) => {
            result.connect_latency_ms = start.elapsed().as_millis() as u64;
            println!("  ✓ 連接成功 ({}ms)", result.connect_latency_ms);
            (stream, resp)
        }
        Ok(Err(e)) => {
            result.error = Some(format!("{}", e));
            println!("  ✗ 連接失敗: {}", e);
            return result;
        }
        Err(_) => {
            result.error = Some("超時".to_string());
            println!("  ✗ 連接超時");
            return result;
        }
    };

    // Lighter 訂閱格式
    let subscribe_msg = serde_json::json!({
        "op": "subscribe",
        "channel": "orderbook",
        "market_id": 1
    });
    ws_stream.send(Message::Text(subscribe_msg.to_string())).await.ok();

    let _ = timeout(Duration::from_secs(5), async {
        while result.messages_received < 3 {
            if let Some(Ok(Message::Text(_))) = ws_stream.next().await {
                result.messages_received += 1;
            }
        }
    }).await;

    result.success = result.messages_received > 0;
    println!("  收到 {} 條消息\n", result.messages_received);
    ws_stream.close(None).await.ok();
    result
}
