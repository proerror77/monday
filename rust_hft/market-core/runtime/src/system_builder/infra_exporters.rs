//! 基礎設施導出模組
//! - Redis 導出（條件編譯 feature = "redis"）
//! - ClickHouse 導出與資料列結構（條件編譯 feature = "clickhouse"）

use super::SystemRuntime;

#[cfg(feature = "redis")]
mod redis_export {
    use super::*;
    use engine::aggregation::TopNSnapshot;
    use tracing::info;

    /// 啟動 Redis 導出任務（從 system_builder.rs 拆分）
    pub async fn spawn_redis_exporter(
        this: &mut SystemRuntime,
        redis_config: super::super::RedisConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 計算中間價格
        fn calculate_mid_price(orderbook: &TopNSnapshot) -> Option<f64> {
            if !orderbook.bid_prices.is_empty() && !orderbook.ask_prices.is_empty() {
                let best_bid = orderbook.bid_prices[0].to_f64();
                let best_ask = orderbook.ask_prices[0].to_f64();
                Some((best_bid + best_ask) / 2.0)
            } else {
                None
            }
        }
        // 計算價差
        fn calculate_spread(orderbook: &TopNSnapshot) -> Option<f64> {
            if !orderbook.bid_prices.is_empty() && !orderbook.ask_prices.is_empty() {
                let best_bid = orderbook.bid_prices[0].to_f64();
                let best_ask = orderbook.ask_prices[0].to_f64();
                Some(best_ask - best_bid)
            } else {
                None
            }
        }

        use redis::{AsyncCommands, Client};

        info!("啟動 Redis 導出器，連接到: {}", redis_config.url);

        // 測試連接
        let client = Client::open(redis_config.url.as_str())?;
        let mut conn = client.get_async_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        info!("Redis 連接測試成功");

        // 克隆引擎引用以供任務使用
        let engine_arc = this.engine.clone();
        let redis_url = redis_config.url.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
            let client = match Client::open(redis_url.as_str()) {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!("Redis 客戶端創建失敗: {}", e);
                    return;
                }
            };

            loop {
                interval.tick().await;
                // 如果引擎已停止，退出任務
                {
                    let eng = engine_arc.lock().await;
                    if !eng.get_statistics().is_running {
                        break;
                    }
                }

                // 獲取當前市場視圖
                let market_view = {
                    let engine = engine_arc.lock().await;
                    engine.get_market_view()
                };

                // 連接 Redis 並導出數據
                match client.get_async_connection().await {
                    Ok(mut conn) => {
                        for (vs, orderbook) in &market_view.orderbooks {
                            let snapshot_data = serde_json::json!({
                                "symbol": vs.symbol.0,
                                "venue": vs.venue.as_str(),
                                "mid_price": calculate_mid_price(orderbook),
                                "spread": calculate_spread(orderbook),
                                "timestamp": market_view.timestamp,
                                "bid_levels": orderbook.bid_prices.len(),
                                "ask_levels": orderbook.ask_prices.len(),
                                "version": market_view.version
                            });

                            let _res: Result<String, redis::RedisError> = conn.xadd(
                                "market_snapshots",
                                "*",
                                &[
                                    ("symbol", vs.symbol.0.as_str()),
                                    ("venue", vs.venue.as_str()),
                                    ("data", snapshot_data.to_string().as_str()),
                                ],
                            ).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Redis 連接失敗: {}", e);
                    }
                }
            }
        });

        this.tasks.push(handle);
        Ok(())
    }

    impl SystemRuntime {
        #[cfg(feature = "redis")]
        pub async fn spawn_redis_exporter(
            &mut self,
            redis_config: super::super::RedisConfig,
        ) -> Result<(), Box<dyn std::error::Error>> {
            spawn_redis_exporter(self, redis_config).await
        }
    }
}

#[cfg(feature = "clickhouse")]
mod clickhouse_export {
    use super::*;
    use clickhouse::Client;
    use tracing::info;

    // ClickHouse 插入行結構（從 system_builder.rs 移動至此）
    #[derive(clickhouse::Row, serde::Serialize, serde::Deserialize, Debug)]
    struct LobDepthRow {
        timestamp: u64,
        symbol: String,
        venue: String,
        side: String, // "bid" or "ask"
        level: u32,   // 0 = best, 1 = second, etc.
        price: f64,
        quantity: f64,
    }

    #[derive(clickhouse::Row, serde::Serialize, serde::Deserialize, Debug)]
    struct EngineStatsRow {
        timestamp: u64,
        orders_submitted: u64,
        orders_filled: u64,
        delta_submitted: u64,
        delta_filled: u64,
        fill_rate: f64,
        execution_events_processed: u64,
    }

    #[derive(clickhouse::Row, serde::Serialize, serde::Deserialize, Debug)]
    struct FactorRow {
        timestamp: u64,
        symbol: String,
        venue: String,
        obi_l1: f64,
        obi_l5: f64,
        spread_bps: f64,
        microprice: f64,
        depth_ratio_l5: f64,
        ofi_l1: f64,
        ofi_l5: f64,
        mid_change_bps: f64,
    }

    /// 啟動 ClickHouse Writer 任務（從 system_builder.rs 拆分）
    pub async fn spawn_clickhouse_writer(
        this: &mut SystemRuntime,
        clickhouse_config: super::super::ClickHouseConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ch_url = clickhouse_config.url.clone();
        let ch_db = clickhouse_config.database.clone().unwrap_or_else(|| "default".into());
        let client = Client::default().with_url(&ch_url).with_database(&ch_db);

        // 檢查連線
        if let Err(e) = client.ping().await {
            tracing::warn!("ClickHouse Writer: ping 失敗: {}", e);
        }

        // LOB 深度 Writer（每 200ms）
        let engine_arc = this.engine.clone();
        let client_for_lob = Client::default().with_url(&ch_url).with_database(&ch_db);
        let l1_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(200));
            let mut prev_map: std::collections::HashMap<String, (f64, f64, f64, f64, f64, f64, f64)> = Default::default();
            loop {
                ticker.tick().await;
                // 若引擎停止則退出
                let (mv, running) = {
                    let eng = engine_arc.lock().await;
                    (eng.get_market_view(), eng.get_statistics().is_running)
                };
                if !running { break; }

                // 批量轉換 L1/L5 深度
                let mut batch = Vec::<LobDepthRow>::new();
                for (vs, orderbook) in &mv.orderbooks {
                    let ts = mv.timestamp;
                    let venue = vs.venue.as_str().to_string();
                    // L1
                    if !orderbook.bid_prices.is_empty() {
                        batch.push(LobDepthRow {
                            timestamp: ts,
                            symbol: vs.symbol.0.clone(),
                            venue: venue.clone(),
                            side: "bid".into(),
                            level: 0,
                            price: orderbook.bid_prices[0].to_f64(),
                            quantity: orderbook.bid_qtys[0].to_f64(),
                        });
                    }
                    if !orderbook.ask_prices.is_empty() {
                        batch.push(LobDepthRow {
                            timestamp: ts,
                            symbol: vs.symbol.0.clone(),
                            venue: venue.clone(),
                            side: "ask".into(),
                            level: 0,
                            price: orderbook.ask_prices[0].to_f64(),
                            quantity: orderbook.ask_qtys[0].to_f64(),
                        });
                    }
                }
                if !batch.is_empty() {
                    if let Ok(mut inserter) = client_for_lob.insert::<LobDepthRow>("hft.lob_depth") {
                        for r in &batch { let _ = inserter.write(r).await; }
                        let _ = inserter.end().await;
                    }
                }
            }
        });
        this.tasks.push(l1_handle);
        info!("ClickHouse L1 per-venue Writer 已啟動（hft.l1_venue）");

        // 主 Writer：市場因子 + 引擎統計（每秒）
        let engine_arc = this.engine.clone();
        let client = Client::default().with_url(&ch_url).with_database(&ch_db);
        let factors_client = Client::default().with_url(&ch_url).with_database(&ch_db);
        let handle = tokio::spawn(async move {
            let mut prev_submitted: u64 = 0;
            let mut prev_filled: u64 = 0;
            let mut prev_exec_processed: u64 = 0;
            let mut prev_map: std::collections::HashMap<String, (f64, f64, f64, f64, f64, f64, f64)> = Default::default();
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                ticker.tick().await;
                let (market_view, running) = {
                    let eng = engine_arc.lock().await;
                    (eng.get_market_view(), eng.get_statistics().is_running)
                };
                if !running { break; }

                let ts = market_view.timestamp;
                let mut f_rows: Vec<FactorRow> = Vec::new();
                // 計算市場因子
                for (vs, ob) in &market_view.orderbooks {
                    let key = vs.symbol.0.clone();
                    let mut bid_px = 0.0;
                    let mut ask_px = 0.0;
                    let mut bid_qty = 0.0;
                    let mut ask_qty = 0.0;
                    let mut bid_qty_l5 = 0.0;
                    let mut ask_qty_l5 = 0.0;
                    if !ob.bid_prices.is_empty() { bid_px = ob.bid_prices[0].to_f64(); bid_qty = ob.bid_qtys[0].to_f64(); }
                    if !ob.ask_prices.is_empty() { ask_px = ob.ask_prices[0].to_f64(); ask_qty = ob.ask_qtys[0].to_f64(); }
                    for i in 0..ob.bid_prices.len().min(5) { bid_qty_l5 += ob.bid_qtys[i].to_f64(); }
                    for i in 0..ob.ask_prices.len().min(5) { ask_qty_l5 += ob.ask_qtys[i].to_f64(); }
                    let mid = if bid_px > 0.0 && ask_px > 0.0 { (bid_px + ask_px) / 2.0 } else { 0.0 };
                    let spread_bps = if mid > 0.0 { (ask_px - bid_px) / mid * 10000.0 } else { 0.0 };
                    let obi1 = if (bid_qty + ask_qty) > 0.0 { (bid_qty - ask_qty) / (bid_qty + ask_qty) } else { 0.0 };
                    let obi5 = if (bid_qty_l5 + ask_qty_l5) > 0.0 { (bid_qty_l5 - ask_qty_l5) / (bid_qty_l5 + ask_qty_l5) } else { 0.0 };
                    let (mut ofi1, mut ofi5, mut mid_change_bps) = (0.0, 0.0, 0.0);
                    if let Some((pb, pa, qb, qa, qb5, qa5, pmid)) = prev_map.get(&key).copied() {
                        ofi1 = (bid_px - pb) * qb + (qb - qb) * pb - (ask_px - pa) * qa - (qa - qa) * pa;
                        ofi5 = (bid_qty_l5 - qb5) - (ask_qty_l5 - qa5);
                        if pmid > 0.0 && mid > 0.0 { mid_change_bps = (mid - pmid) / pmid * 10000.0; }
                    }
                    prev_map.insert(key.clone(), (bid_px, ask_px, bid_qty, ask_qty, bid_qty_l5, ask_qty_l5, mid));
                    f_rows.push(FactorRow {
                        timestamp: ts,
                        symbol: key,
                        venue: vs.venue.as_str().into(),
                        obi_l1: obi1,
                        obi_l5: obi5,
                        spread_bps,
                        microprice: if (bid_qty + ask_qty) > 0.0 { (ask_px * bid_qty + bid_px * ask_qty) / (bid_qty + ask_qty) } else { mid },
                        depth_ratio_l5: if ask_qty_l5 > 0.0 { bid_qty_l5 / ask_qty_l5 } else { 0.0 },
                        ofi_l1: ofi1,
                        ofi_l5: ofi5,
                        mid_change_bps,
                    });
                }
                if !f_rows.is_empty() {
                    if let Ok(mut inserter) = factors_client.insert::<FactorRow>("hft.factors") {
                        for r in &f_rows { let _ = inserter.write(r).await; }
                        let _ = inserter.end().await;
                    }
                }

                // 引擎統計（每秒一條）
                let stats = {
                    let eng = engine_arc.lock().await;
                    eng.get_statistics()
                };
                let delta_submitted = stats.orders_submitted.saturating_sub(prev_submitted);
                let delta_filled = stats.orders_filled.saturating_sub(prev_filled);
                let fill_rate = if delta_submitted > 0 { (delta_filled as f64) / (delta_submitted as f64) } else { 0.0 };
                let row = EngineStatsRow {
                    timestamp: market_view.timestamp,
                    orders_submitted: stats.orders_submitted,
                    orders_filled: stats.orders_filled,
                    delta_submitted,
                    delta_filled,
                    fill_rate,
                    execution_events_processed: stats.execution_events_processed,
                };
                prev_submitted = stats.orders_submitted;
                prev_filled = stats.orders_filled;
                prev_exec_processed = stats.execution_events_processed;

                if let Ok(mut inserter) = client.insert::<EngineStatsRow>("hft.engine_stats") {
                    let _ = inserter.write(&row).await;
                    let _ = inserter.end().await;
                }
            }
        });
        this.tasks.push(handle);
        info!("ClickHouse Writer 已啟動，每秒批量寫入 lob_depth 表");

        // 交易流 Writer（每秒刷批）
        let engine_for_trades = this.engine.clone();
        let trades_client = Client::default().with_url(&ch_url).with_database(&ch_db);
        let trade_handle = tokio::spawn(async move {
            let mut rx = { let eng = engine_for_trades.lock().await; eng.subscribe_market_trades() };
            let mut batch: Vec<(u64, String, String, String, f64, f64, String)> = Vec::with_capacity(1024);
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if !batch.is_empty() {
                            if let Ok(mut inserter) = trades_client.insert::<(u64, String, String, String, f64, f64, String)>("hft.trade_data") {
                                for row in &batch { let _ = inserter.write(row).await; }
                                let _ = inserter.end().await;
                            }
                            batch.clear();
                        }
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(tr) => {
                                batch.push((tr.ts, tr.symbol.0.clone(), tr.venue.as_str().into(), tr.side.as_str().into(), tr.price.to_f64(), tr.quantity.to_f64(), tr.liq.as_str().into()));
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });
        this.tasks.push(trade_handle);
        Ok(())
    }

    impl SystemRuntime {
        #[cfg(feature = "clickhouse")]
        pub async fn spawn_clickhouse_writer(
            &mut self,
            clickhouse_config: super::super::ClickHouseConfig,
        ) -> Result<(), Box<dyn std::error::Error>> {
            spawn_clickhouse_writer(self, clickhouse_config).await
        }
    }
}
