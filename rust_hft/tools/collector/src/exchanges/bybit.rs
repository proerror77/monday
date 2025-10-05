use super::{Exchange, ExchangeContext, MessageBuffers};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub struct BybitExchange {
    #[allow(dead_code)]
    ctx: Arc<ExchangeContext>,
}

impl BybitExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Exchange for BybitExchange {
    fn name(&self) -> &'static str {
        "bybit"
    }

    fn websocket_url(&self) -> String {
        // v5 公共通道；Bybit 需要按類別使用具體路徑。
        // 官方文檔：
        //   Spot:    wss://stream.bybit.com/v5/public/spot
        //   Linear:  wss://stream.bybit.com/v5/public/linear
        //   Inverse: wss://stream.bybit.com/v5/public/inverse
        // 這裡默認採用現貨端點（可用環境變數 BYBIT_WS_URL 覆蓋）。
        std::env::var("BYBIT_WS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "wss://stream.bybit.com/v5/public/spot".to_string())
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        // v5 24hr tickers（根據 BYBIT_CATEGORY 選擇 spot 或 linear）
        let category = std::env::var("BYBIT_CATEGORY")
            .unwrap_or_else(|_| "spot".to_string())
            .to_lowercase();
        #[derive(Deserialize)]
        struct BybitTickerItem {
            symbol: String,
            #[serde(rename = "turnover24h")]
            turnover24h: String,
        }
        #[derive(Deserialize)]
        struct BybitTickerRespResult {
            list: Vec<BybitTickerItem>,
        }
        #[derive(Deserialize)]
        struct BybitTickerResp {
            result: BybitTickerRespResult,
        }

        let url = if matches!(category.as_str(), "linear" | "usdt" | "perp") {
            "https://api.bybit.com/v5/market/tickers?category=linear"
        } else {
            "https://api.bybit.com/v5/market/tickers?category=spot"
        };
        let client = reqwest::Client::new();
        let resp: BybitTickerResp = client
            .get(url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?;

        let mut v: Vec<(String, f64)> = resp
            .result
            .list
            .into_iter()
            .filter_map(|it| it.turnover24h.parse::<f64>().ok().map(|v| (it.symbol, v)))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        Ok(v.into_iter().take(limit).map(|(s, _)| s).collect())
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        let create_orderbook_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bybit_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Float64,
                qty Float64,
                update_id Int64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook_table).execute().await?;

        let create_trades_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bybit_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Float64,
                qty Float64,
                side Enum8('buy'=1, 'sell'=2)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades_table).execute().await?;

        // 24hr mini ticker 表
        let create_ticker_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bybit_ticker (
                ts DateTime64(3),
                symbol LowCardinality(String),
                last Float64,
                open Float64,
                high Float64,
                low Float64,
                volume Float64,
                quote_volume Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            "#,
            database
        );
        client.query(&create_ticker_table).execute().await?;

        // L1 表：最優買賣檔，方便快速查詢 BBO
        let create_l1_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.bybit_l1 (
                ts DateTime64(3),
                symbol LowCardinality(String),
                bid_px Float64,
                bid_qty Float64,
                ask_px Float64,
                ask_qty Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts)
            "#,
            database
        );
        client.query(&create_l1_table).execute().await?;

        Ok(())
    }

    async fn process_message(&self, message: &str, buffers: &mut MessageBuffers) -> Result<()> {
        // v5: {"topic":"orderbook.1.BTCUSDT","type":"delta","data":{...}}
        let v: Value = match serde_json::from_str(message) {
            Ok(val) => val,
            Err(_) => return Ok(()),
        };
        // 調試：訂閱回覆/錯誤資訊（一次性建議，觀察現網行為）
        if let Some(op) = v.get("op").and_then(|x| x.as_str()) {
            let success = v.get("success").and_then(|x| x.as_bool());
            let ret_msg = v.get("ret_msg").and_then(|x| x.as_str());
            tracing::info!(
                "bybit ws op={}, success={:?}, ret_msg={:?}, msg={}",
                op,
                success,
                ret_msg,
                message
            );
            return Ok(());
        }
        if v.get("ret_code").is_some() || v.get("code").is_some() || v.get("ret_msg").is_some() {
            tracing::warn!("bybit response: {}", message);
        }
        let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("");
        if topic.starts_with("orderbook.") {
            if let Some(data) = v.get("data") {
                let symbol = data.get("s").and_then(|x| x.as_str()).unwrap_or("");
                let ts = data
                    .get("ts")
                    .or_else(|| v.get("ts"))
                    .and_then(|x| x.as_i64())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                let mut best_bid: Option<(f64, f64)> = None;
                let mut best_ask: Option<(f64, f64)> = None;
                // bids: [[price, size], ...] or with seq
                for (side_key, side_str) in [("b", "bid"), ("a", "ask")] {
                    if let Some(arr) = data.get(side_key).and_then(|x| x.as_array()) {
                        for level in arr {
                            if let Some(px) = level.get(0).and_then(|x| x.as_str()) {
                                let qty = level.get(1).and_then(|x| x.as_str()).unwrap_or("0");
                                if let (Ok(price), Ok(q)) = (px.parse::<f64>(), qty.parse::<f64>())
                                {
                                    let row = serde_json::json!({
                                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                                        "symbol": symbol,
                                        "side": side_str,
                                        "price": price,
                                        "qty": q,
                                        "update_id": 0,
                                    });
                                    buffers.push_spot_orderbook(serde_json::to_string(&row)?);
                                    // 追蹤最佳桿位
                                    if side_str == "bid" {
                                        if best_bid.map(|(p, _)| price > p).unwrap_or(true) {
                                            best_bid = Some((price, q));
                                        }
                                    } else {
                                        if best_ask.map(|(p, _)| price < p).unwrap_or(true) {
                                            best_ask = Some((price, q));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let (Some((bp, bq)), Some((ap, aq))) = (best_bid, best_ask) {
                    let l1 = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": symbol,
                        "bid_px": bp,
                        "bid_qty": bq,
                        "ask_px": ap,
                        "ask_qty": aq,
                    });
                    buffers.push_spot_l1(serde_json::to_string(&l1)?);
                }
            }
        } else if topic.starts_with("publicTrade.") {
            if let Some(list) = v.get("data").and_then(|x| x.as_array()) {
                for t in list {
                    let symbol = t.get("s").and_then(|x| x.as_str()).unwrap_or("");
                    let price = t
                        .get("p")
                        .and_then(|x| x.as_str())
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let qty = t
                        .get("q")
                        .and_then(|x| x.as_str())
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let side = t
                        .get("S")
                        .and_then(|x| x.as_str())
                        .unwrap_or("Buy")
                        .to_lowercase();
                    let trade_id = t
                        .get("i")
                        .or_else(|| t.get("t"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let ts = t
                        .get("T")
                        .and_then(|x| x.as_i64())
                        .unwrap_or_else(|| Utc::now().timestamp_millis());
                    let row = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": symbol,
                        "trade_id": trade_id,
                        "price": price,
                        "qty": qty,
                        "side": if side.starts_with('b'){ "buy" } else { "sell" },
                    });
                    buffers.push_spot_trades(serde_json::to_string(&row)?);
                }
            }
        } else if topic.starts_with("tickers.") {
            if let Some(t) = v.get("data") {
                let symbol = t.get("symbol").and_then(|x| x.as_str()).unwrap_or("");
                let ts = t
                    .get("ts")
                    .and_then(|x| x.as_i64())
                    .unwrap_or_else(|| Utc::now().timestamp_millis());
                let last = t
                    .get("lastPrice")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let open = t
                    .get("open24h")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let high = t
                    .get("high24h")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let low = t
                    .get("low24h")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let volume = t
                    .get("volume24h")
                    .or_else(|| t.get("v"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let quote_volume = t
                    .get("turnover24h")
                    .or_else(|| t.get("qv"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let row = serde_json::json!({
                    "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                    "symbol": symbol,
                    "last": last,
                    "open": open,
                    "high": high,
                    "low": low,
                    "volume": volume,
                    "quote_volume": quote_volume,
                });
                buffers.push_spot_ticker(serde_json::to_string(&row)?);

                // 由 tickers 提供的 BBO 欄位直接寫入 L1（若存在）
                let bid_px = t
                    .get("bid1Price")
                    .or_else(|| t.get("bidPrice"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok());
                let bid_qty = t
                    .get("bid1Size")
                    .or_else(|| t.get("bidSize"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok());
                let ask_px = t
                    .get("ask1Price")
                    .or_else(|| t.get("askPrice"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok());
                let ask_qty = t
                    .get("ask1Size")
                    .or_else(|| t.get("askSize"))
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<f64>().ok());
                if let (Some(bp), Some(bq), Some(ap), Some(aq)) = (bid_px, bid_qty, ask_px, ask_qty)
                {
                    let l1 = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": symbol,
                        "bid_px": bp,
                        "bid_qty": bq,
                        "ask_px": ap,
                        "ask_qty": aq,
                    });
                    buffers.push_spot_l1(serde_json::to_string(&l1)?);
                }
            }
        }
        Ok(())
    }

    async fn flush_data(
        &self,
        database: &str,
        buffers: &mut MessageBuffers,
    ) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;

        // orderbook
        buffers
            .flush_spot_table(&sink, "bybit_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "bybit_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "bybit_ticker", |spot| &mut spot.ticker)
            .await?;
        buffers
            .flush_spot_table(&sink, "bybit_l1", |spot| &mut spot.l1)
            .await?;
        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        // Bybit v5 對單次 subscribe 的 args 長度有限制（文檔建議分批，實測 10-30 比較安全）
        // 支持通過環境變數覆蓋分批大小：BYBIT_SUB_CHUNK
        let chunk_size = std::env::var("BYBIT_SUB_CHUNK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(10);

        let levels: usize = std::env::var("DEPTH_LEVELS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(50);

        // 兩種風格：字串 topic 或 對象參數。可透過 BYBIT_SUB_STYLE=string|object 指定（預設 object）。
        let use_object_args = std::env::var("BYBIT_SUB_STYLE")
            .map(|v| v.eq_ignore_ascii_case("object"))
            .unwrap_or(true);
        let category = std::env::var("BYBIT_CATEGORY")
            .unwrap_or_else(|_| "spot".to_string())
            .to_lowercase();
        let mut msgs = Vec::new();
        if use_object_args {
            // 對象風格：{"op":"subscribe","args":[{"topic":"tickers","params":{"category":"spot","symbol":"BTCUSDT"}}, ...]}
            let mut args_obj = Vec::new();
            for s in symbols {
                args_obj.push(serde_json::json!({
                    "topic": "orderbook",
                    "params": {"category": category, "symbol": s, "depth": levels}
                }));
                // 額外訂閱 orderbook.1 作為 BBO 來源
                args_obj.push(serde_json::json!({
                    "topic": "orderbook",
                    "params": {"category": category, "symbol": s, "depth": 1}
                }));
                args_obj.push(serde_json::json!({
                    "topic": "publicTrade",
                    "params": {"category": category, "symbol": s}
                }));
                args_obj.push(serde_json::json!({
                    "topic": "tickers",
                    "params": {"category": category, "symbol": s}
                }));
            }
            for chunk in args_obj.chunks(chunk_size) {
                let sub_msg = serde_json::json!({"op":"subscribe","args": chunk});
                msgs.push(sub_msg.to_string());
            }
            Ok(msgs)
        } else {
            let args_list: Vec<String> = symbols
                .iter()
                .flat_map(|s| {
                    vec![
                        format!("orderbook.{}.{}", levels, s),
                        format!("orderbook.1.{}", s),
                        format!("publicTrade.{}", s),
                        format!("tickers.{}", s),
                    ]
                })
                .collect();
            for chunk in args_list.chunks(chunk_size) {
                let sub_msg = serde_json::json!({"op":"subscribe","args": chunk});
                msgs.push(sub_msg.to_string());
            }
            Ok(msgs)
        }
    }
}
