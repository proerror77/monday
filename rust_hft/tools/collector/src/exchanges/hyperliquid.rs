use super::{hyperliquid_pairs, Exchange, ExchangeContext, MessageBuffers};
use anyhow::Result;
use async_trait::async_trait;
use chrono::prelude::*;
use clickhouse::Client;
use serde_json::Value;
use std::sync::Arc;

pub struct HyperliquidExchange {
    #[allow(dead_code)]
    ctx: Arc<ExchangeContext>,
}

impl HyperliquidExchange {
    pub fn new(ctx: Arc<ExchangeContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    fn name(&self) -> &'static str {
        "hyperliquid"
    }

    fn websocket_url(&self) -> String {
        std::env::var("HYPERLIQUID_WS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "wss://api.hyperliquid.xyz/ws".to_string())
    }

    fn websocket_headers(&self) -> Vec<(String, String)> {
        // 直连时按需附带 Origin 头
        vec![
            (
                "Origin".to_string(),
                std::env::var("HYPERLIQUID_ORIGIN")
                    .unwrap_or_else(|_| "https://app.hyperliquid.xyz".to_string()),
            ),
            (
                "User-Agent".to_string(),
                std::env::var("HYPERLIQUID_UA").unwrap_or_else(|_|
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36".to_string()
                ),
            ),
            (
                "Sec-WebSocket-Extensions".to_string(),
                std::env::var("HYPERLIQUID_WS_EXT").unwrap_or_else(|_|
                    "permessage-deflate; client_max_window_bits".to_string()
                ),
            ),
        ]
    }

    fn heartbeat_interval(&self) -> std::time::Duration {
        // 官方建議：60s 無消息需 ping，設置 55s 主動心跳
        std::time::Duration::from_secs(55)
    }

    async fn get_popular_symbols(&self, limit: usize) -> Result<Vec<String>> {
        if let Some(symbols) = self.ctx.symbols_override() {
            return Ok((*symbols).clone());
        }

        let mut whitelist: Vec<String> = hyperliquid_pairs()
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect();
        whitelist.sort();
        whitelist.dedup();

        if limit > 0 && limit < whitelist.len() {
            whitelist.truncate(limit);
        }

        Ok(whitelist)
    }

    async fn setup_tables(&self, client: &Client, database: &str) -> Result<()> {
        let create_orderbook_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_orderbook (
                ts DateTime64(3),
                symbol LowCardinality(String),
                side Enum8('bid'=1, 'ask'=2),
                price Decimal64(10),
                qty Decimal64(10)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, side, price)
            "#,
            database
        );
        client.query(&create_orderbook_table).execute().await?;

        let create_trades_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_trades (
                ts DateTime64(3),
                symbol LowCardinality(String),
                trade_id String,
                price Decimal64(10),
                qty Decimal64(10),
                side Enum8('buy'=1, 'sell'=2)
            ) ENGINE = MergeTree()
            ORDER BY (symbol, ts, trade_id)
            "#,
            database
        );
        client.query(&create_trades_table).execute().await?;

        // L1 表（衍生自當前更新的最佳買賣）
        let create_l1_table = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {}.hyperliquid_l1 (
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
        let v: Value = match serde_json::from_str(message) {
            Ok(x) => x,
            Err(_) => return Ok(()),
        };
        let channel = v.get("channel").and_then(|x| x.as_str()).unwrap_or("");
        if channel.eq_ignore_ascii_case("subscriptionResponse")
            || channel.eq_ignore_ascii_case("pong")
        {
            return Ok(());
        }
        if channel == "l2Book" {
            if let Some(data) = v.get("data") {
                let symbol = data.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                if let Some(levels) = data.get("levels").and_then(|x| x.as_array()) {
                    let ts = Utc::now().timestamp_millis();
                    let mut best_bid: Option<(f64, f64)> = None;
                    let mut best_ask: Option<(f64, f64)> = None;
                    for lvl in levels {
                        let px = lvl.get(0).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let qty = lvl.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let side = lvl.get(2).and_then(|x| x.as_str()).unwrap_or("b");
                        let row = serde_json::json!({
                            "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                            "symbol": format!("{}-USDT", symbol),
                            "side": if side.starts_with('b'){"bid"} else {"ask"},
                            "price": px,
                            "qty": qty,
                            "update_id": 0,
                        });
                        buffers.push_spot_orderbook(serde_json::to_string(&row)?);

                        // 追蹤最佳買賣
                        if side.eq_ignore_ascii_case("b") {
                            if best_bid.map(|(p, _)| px > p).unwrap_or(true) {
                                best_bid = Some((px, qty));
                            }
                        } else {
                            if best_ask.map(|(p, _)| px < p).unwrap_or(true) {
                                best_ask = Some((px, qty));
                            }
                        }
                    }
                    if let (Some((bp, bq)), Some((ap, aq))) = (best_bid, best_ask) {
                        let l1 = serde_json::json!({
                            "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                            "symbol": format!("{}-USDT", symbol),
                            "bid_px": bp,
                            "bid_qty": bq,
                            "ask_px": ap,
                            "ask_qty": aq,
                        });
                        buffers.push_spot_l1(serde_json::to_string(&l1)?);
                    }
                }
            }
        } else if channel == "trades" {
            if let Some(list) = v.get("data").and_then(|x| x.as_array()) {
                for tr in list {
                    let symbol = tr.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                    let px = tr.get("px").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let sz = tr.get("sz").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let side = tr.get("side").and_then(|x| x.as_str()).unwrap_or("B");
                    let ts = tr
                        .get("time")
                        .and_then(|x| x.as_i64())
                        .unwrap_or_else(|| Utc::now().timestamp_millis());
                    let tid = tr
                        .get("tid")
                        .or_else(|| tr.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let row = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": format!("{}-USDT", symbol),
                        "trade_id": tid,
                        "price": px,
                        "qty": sz,
                        "side": if side.to_lowercase().starts_with('b'){"buy"} else {"sell"},
                    });
                    buffers.push_spot_trades(serde_json::to_string(&row)?);
                }
            }
        } else if channel == "bbo" {
            if let Some(obj) = v.get("data").and_then(|x| x.as_object()) {
                let coin = obj.get("coin").and_then(|x| x.as_str()).unwrap_or("");
                let ts = Utc::now().timestamp_millis();
                // 支持多種字段命名
                let bid_px = obj
                    .get("bp")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("bidPx").and_then(|x| x.as_f64()));
                let bid_qty = obj
                    .get("bq")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("bidSz").and_then(|x| x.as_f64()));
                let ask_px = obj
                    .get("ap")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("askPx").and_then(|x| x.as_f64()));
                let ask_qty = obj
                    .get("aq")
                    .and_then(|x| x.as_f64())
                    .or_else(|| obj.get("askSz").and_then(|x| x.as_f64()));
                if let (Some(bp), Some(bq), Some(ap), Some(aq)) = (bid_px, bid_qty, ask_px, ask_qty)
                {
                    let l1 = serde_json::json!({
                        "ts": chrono::DateTime::<Utc>::from_timestamp_millis(ts).unwrap(),
                        "symbol": format!("{}-USDT", coin),
                        "bid_px": bp,
                        "bid_qty": bq,
                        "ask_px": ap,
                        "ask_qty": aq,
                    });
                    buffers.push_spot_l1(serde_json::to_string(&l1)?);
                }
            }
        } else if channel == "allMids" {
            // 可選：忽略或寫入專用表；當前忽略。
        }
        Ok(())
    }

    async fn flush_data(
        &self,
        database: &str,
        buffers: &mut MessageBuffers,
    ) -> Result<()> {
        let sink = crate::db::get_sink_async(database).await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_orderbook", |spot| &mut spot.orderbook)
            .await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_trades", |spot| &mut spot.trades)
            .await?;
        buffers
            .flush_spot_table(&sink, "hyperliquid_l1", |spot| &mut spot.l1)
            .await?;
        let _ = crate::spool::drain(&sink).await;
        Ok(())
    }

    async fn subscription_messages(&self, symbols: &[String]) -> Result<Vec<String>> {
        // 額外主題透過環境變數配置，預設訂閱 l2Book,trades
        let topics_csv = std::env::var("HYPERLIQUID_WS_TOPICS")
            .unwrap_or_else(|_| "l2Book,trades,bbo".to_string());
        let topics: Vec<String> = topics_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let coins: Vec<String> = symbols
            .iter()
            .map(|sym| {
                if let Some(pos) = sym.find('-') {
                    sym[..pos].to_string()
                } else if sym.ends_with("USDT") {
                    sym.trim_end_matches("USDT").to_string()
                } else {
                    sym.clone()
                }
            })
            .collect();

        let mut msgs = Vec::new();
        for coin in coins {
            for topic in &topics {
                if topic == "allMids" {
                    // 全市場中價不需要 coin
                    let sub = serde_json::json!({
                        "method":"subscribe",
                        "subscription": {"type": "allMids"}
                    });
                    msgs.push(sub.to_string());
                    continue;
                }
                let sub = serde_json::json!({
                    "method":"subscribe",
                    "subscription": {"type": topic, "coin": coin}
                });
                msgs.push(sub.to_string());
            }
        }
        Ok(msgs)
    }
}
