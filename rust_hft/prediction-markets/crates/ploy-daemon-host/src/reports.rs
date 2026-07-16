use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ploy_operator_contracts::reports::{DryRunHourlyRow, DryRunHourlyWindowRow};
use ploy_operator_contracts::{
    DryRunClosedTradeRow, DryRunDailyRow, DryRunDailyWindowRow, DryRunEquityPoint,
    DryRunExecutionDiagnostics, DryRunMetrics, DryRunOpenPositionRow, DryRunPairingReport,
    DryRunPerformanceReport, DryRunRuntimeEvidence, DryRunStrategyReport, DryRunSummary,
    DryRunSymbolRow, DryRunWindowRow, NumberOrText,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::time::timeout;

const DEFAULT_DATABASE_URL: &str = "postgresql://postgres:postgres@localhost:5432/ploy";
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

const EVENTS_QUERY: &str = r#"
WITH events AS (
  SELECT
    t.runtime_mode,
    t.strategy_id,
    t.deployment_id,
    t.trade_key,
    t.event_id,
    t.intent_id,
    t.symbol,
    t.token_id,
    t.market_side,
    t.opened_at,
    t.closed_at,
    t.last_fill_at,
    t.fill_count,
    t.buy_quantity,
    t.buy_notional,
    t.total_fee,
    t.avg_entry_price,
    t.avg_exit_price,
    t.gross_pnl,
    t.net_pnl,
    t.is_closed,
    t.open_quantity,
    CASE
      WHEN ms.market_slug IS NOT NULL THEN 'token_settlement_market_metadata'
      WHEN s.market_slug IS NOT NULL THEN 'token_settlement_without_metadata'
      WHEN me.market_slug IS NOT NULL THEN 'event_track_market_metadata'
      WHEN mt.market_slug IS NOT NULL THEN 'trade_key_market_metadata'
      ELSE 'missing_market_metadata'
    END AS metadata_join_status,
    COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) AS metadata_market_slug,
    CASE
      WHEN COALESCE(ms.end_time, me.end_time, mt.end_time) IS NOT NULL
        AND COALESCE(ms.start_time, me.start_time, mt.start_time) IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (
          COALESCE(ms.end_time, me.end_time, mt.end_time)
          - COALESCE(ms.start_time, me.start_time, mt.start_time)
        )))::int
      WHEN COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%15m%'
        OR COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%15-minute%'
        THEN 900
      WHEN COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%5m%'
        OR COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%5-minute%'
        THEN 300
      ELSE NULL
    END AS window_secs,
    CASE
      WHEN COALESCE(ms.end_time, me.end_time, mt.end_time) IS NOT NULL
        AND t.opened_at IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (COALESCE(ms.end_time, me.end_time, mt.end_time) - t.opened_at)))::int
      ELSE NULL
    END AS entry_time_remaining_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_token_settlements s ON s.token_id = t.token_id
  LEFT JOIN pm_market_metadata ms ON ms.market_slug = s.market_slug
  LEFT JOIN pm_market_metadata me ON me.market_slug = t.event_id
  LEFT JOIN pm_market_metadata mt ON mt.market_slug = t.trade_key
  WHERE t.runtime_mode IN ('dry_run', 'dryrun', 'paper')
    AND ($1::date IS NULL OR COALESCE(t.closed_at, t.opened_at) >= ($1::date AT TIME ZONE 'Asia/Shanghai'))
)
SELECT COALESCE(json_agg(row_to_json(e) ORDER BY e.opened_at, e.trade_key), '[]'::json)::text
FROM events e;
"#;

const DAILY_QUERY: &str = r#"
SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst, d.runtime_mode, d.strategy_id, d.deployment_id), '[]'::json)::text
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    trading_day_cst,
    trade_count,
    closed_trade_count,
    winning_trade_count_all AS wins,
    losing_trade_count_all AS losses,
    confirmed_trade_count,
    net_pnl,
    confirmed_net_pnl,
    total_fee AS fees,
    residual_open_quantity AS open_quantity
  FROM strategy_runtime_daily_track_record
  WHERE runtime_mode IN ('dry_run', 'dryrun', 'paper')
    AND ($1::date IS NULL OR trading_day_cst >= $1::date)
) d;
"#;

const PAIRING_QUERY: &str = r#"
SELECT json_build_object(
  'pair_key', 'runtime_mode,strategy_id,deployment_id,event_id',
  'mixed_event_groups', COUNT(*),
  'fills_in_mixed_event_groups', COALESCE(SUM(fill_count), 0),
  'current_view_rows', (
    SELECT COUNT(*) FROM strategy_runtime_event_track_record
    WHERE runtime_mode IN ('dry_run', 'dryrun', 'paper')
  ),
  'side_aware_rows', (
    SELECT COUNT(*) FROM (
      SELECT runtime_mode, strategy_id, deployment_id,
        COALESCE(NULLIF(event_id, ''), intent_id) AS event_or_intent,
        COALESCE(NULLIF(token_id, ''), 'unknown') AS token_key,
        COALESCE(NULLIF(market_side, ''), 'unknown') AS side_key
      FROM strategy_runtime_fills
      WHERE runtime_mode IN ('dry_run', 'dryrun', 'paper')
      GROUP BY runtime_mode, strategy_id, deployment_id, event_or_intent, token_key, side_key
    ) side_groups
  )
)::text
FROM (
  SELECT runtime_mode, strategy_id, deployment_id, event_id, COUNT(*) AS fill_count
  FROM strategy_runtime_fills
  WHERE runtime_mode IN ('dry_run', 'dryrun', 'paper')
    AND event_id IS NOT NULL AND event_id <> ''
  GROUP BY runtime_mode, strategy_id, deployment_id, event_id
  HAVING COUNT(DISTINCT token_id) > 1 OR COUNT(DISTINCT market_side) > 1
) mixed;
"#;

const ORDER_DIAGNOSTICS_QUERY: &str = r#"
SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.runtime_mode, d.strategy_id, d.deployment_id), '[]'::json)::text
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    COUNT(*) AS total_orders,
    COUNT(*) FILTER (WHERE order_side = 'BUY') AS buy_orders,
    COUNT(*) FILTER (WHERE order_side = 'SELL') AS sell_orders,
    COUNT(*) FILTER (WHERE LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL) AS rejected_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY'
        AND (LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL)
    ) AS rejected_buy_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY' AND filled_quantity > 0 AND quantity > 0
        AND filled_quantity < quantity * 0.98
    ) AS partial_buy_orders,
    ROUND(COALESCE(SUM(quantity * COALESCE(limit_price, avg_fill_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 4) AS buy_requested_notional,
    ROUND(COALESCE(SUM(filled_quantity * COALESCE(avg_fill_price, limit_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 4) AS buy_filled_notional,
    ROUND(
      CASE
        WHEN COALESCE(SUM(quantity) FILTER (WHERE order_side = 'BUY'), 0) > 0
          THEN COALESCE(SUM(filled_quantity) FILTER (WHERE order_side = 'BUY'), 0)
               / SUM(quantity) FILTER (WHERE order_side = 'BUY') * 100
        ELSE 0
      END,
      2
    ) AS buy_fill_rate_pct
  FROM strategy_runtime_orders
  WHERE runtime_mode IN ('dry_run', 'dryrun', 'paper')
    AND ($1::date IS NULL OR created_at >= ($1::date AT TIME ZONE 'Asia/Shanghai'))
  GROUP BY runtime_mode, strategy_id, deployment_id
) d;
"#;

const RUNTIME_EVENTS_QUERY: &str = r#"
SELECT COALESCE(jsonb_agg(jsonb_build_object(
  'runtime_mode', o.runtime_mode,
  'strategy_id', o.strategy_id,
  'deployment_id', o.deployment_id,
  'event_id', o.event_id,
  'market_id', o.event_id,
  'intent_id', o.intent_id,
  'order_id', o.order_id,
  'token_id', o.token_id,
  'market_side', o.market_side,
  'side', o.order_side,
  'decision_ts', o.recorded_at,
  'quote', COALESCE(o.limit_price, o.avg_fill_price),
  'signal_inputs', jsonb_build_object(
    'purpose', COALESCE(o.context ->> 'purpose', 'ENTRY'),
    'requested_qty', o.quantity,
    'limit_price', o.limit_price
  ),
  'entry_price', COALESCE(fill.avg_fill_price, o.avg_fill_price, o.limit_price),
  'fill_status', o.status,
  'settlement', CASE
    WHEN COALESCE(track.is_closed, false) THEN COALESCE(track.avg_exit_price::text, 'closed')
    ELSE 'open'
  END,
  'pnl', COALESCE(track.net_pnl, fill.pnl, 0)
) ORDER BY o.recorded_at, o.order_id), '[]'::jsonb)::text
FROM strategy_runtime_orders o
LEFT JOIN LATERAL (
  SELECT
    CASE WHEN COALESCE(SUM(f.quantity), 0) > 0
      THEN SUM(f.quantity * f.price) / SUM(f.quantity) ELSE NULL END AS avg_fill_price,
    SUM(CASE WHEN f.fill_side = 'SELL' THEN f.quantity * f.price
      ELSE -(f.quantity * f.price) END - f.fee) AS pnl
  FROM strategy_runtime_fills f
  WHERE f.runtime_mode = o.runtime_mode
    AND f.strategy_id = o.strategy_id
    AND f.deployment_id = o.deployment_id
    AND f.order_id = o.order_id
) fill ON true
LEFT JOIN strategy_runtime_event_track_record track
  ON track.runtime_mode = o.runtime_mode
  AND track.strategy_id = o.strategy_id
  AND track.deployment_id = o.deployment_id
  AND track.intent_id = o.intent_id
WHERE o.runtime_mode IN ('dry_run', 'dryrun', 'paper');
"#;

const RUNTIME_ORDERS_QUERY: &str = r#"
SELECT COALESCE(jsonb_agg(jsonb_build_object(
  'runtime_mode', o.runtime_mode,
  'strategy_id', o.strategy_id,
  'deployment_id', o.deployment_id,
  'intent_id', o.intent_id,
  'order_id', o.order_id,
  'venue_order_id', o.venue_order_id,
  'event_id', o.event_id,
  'market_id', o.event_id,
  'token_id', o.token_id,
  'market_side', o.market_side,
  'order_side', o.order_side,
  'quantity', o.quantity,
  'requested_qty', o.quantity,
  'limit_price', o.limit_price,
  'filled_quantity', o.filled_quantity,
  'avg_fill_price', o.avg_fill_price,
  'status', o.status,
  'rejection_reason', o.rejection_reason,
  'context', o.context,
  'created_at', o.recorded_at
) ORDER BY o.recorded_at, o.order_id), '[]'::jsonb)::text
FROM strategy_runtime_orders o
WHERE o.runtime_mode IN ('dry_run', 'dryrun', 'paper');
"#;

const RUNTIME_FILLS_QUERY: &str = r#"
SELECT COALESCE(jsonb_agg(jsonb_build_object(
  'runtime_mode', f.runtime_mode,
  'strategy_id', f.strategy_id,
  'deployment_id', f.deployment_id,
  'intent_id', f.intent_id,
  'order_id', f.order_id,
  'fill_id', f.fill_id,
  'event_id', f.event_id,
  'market_id', f.event_id,
  'token_id', f.token_id,
  'market_side', f.market_side,
  'fill_side', f.fill_side,
  'quantity', f.quantity,
  'price', f.price,
  'fee', f.fee,
  'fill_timestamp', f.fill_timestamp
) ORDER BY f.fill_timestamp, f.fill_id), '[]'::jsonb)::text
FROM strategy_runtime_fills f
WHERE f.runtime_mode IN ('dry_run', 'dryrun', 'paper');
"#;

#[derive(Clone, Debug, Default, Deserialize)]
struct EventRow {
    #[serde(default)]
    runtime_mode: String,
    #[serde(default)]
    strategy_id: String,
    #[serde(default)]
    deployment_id: String,
    trade_key: Option<String>,
    event_id: Option<String>,
    symbol: Option<String>,
    market_side: Option<String>,
    opened_at: Option<String>,
    closed_at: Option<String>,
    last_fill_at: Option<String>,
    buy_quantity: Option<f64>,
    buy_notional: Option<f64>,
    total_fee: Option<f64>,
    avg_entry_price: Option<f64>,
    avg_exit_price: Option<f64>,
    net_pnl: Option<f64>,
    #[serde(default)]
    is_closed: bool,
    window_secs: Option<i64>,
    entry_time_remaining_secs: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct DailyInput {
    #[serde(default)]
    runtime_mode: String,
    #[serde(default)]
    strategy_id: String,
    #[serde(default)]
    deployment_id: String,
    trading_day_cst: Option<String>,
    trade_count: Option<f64>,
    closed_trade_count: Option<f64>,
    wins: Option<f64>,
    losses: Option<f64>,
    confirmed_trade_count: Option<f64>,
    net_pnl: Option<f64>,
    confirmed_net_pnl: Option<f64>,
    fees: Option<f64>,
    open_quantity: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, serde::Serialize)]
struct OrderDiagnosticsInput {
    #[serde(default)]
    runtime_mode: String,
    #[serde(default)]
    strategy_id: String,
    #[serde(default)]
    deployment_id: String,
    #[serde(default)]
    total_orders: f64,
    #[serde(default)]
    buy_orders: f64,
    #[serde(default)]
    sell_orders: f64,
    #[serde(default)]
    rejected_orders: f64,
    #[serde(default)]
    rejected_buy_orders: f64,
    #[serde(default)]
    partial_buy_orders: f64,
    #[serde(default)]
    buy_requested_notional: f64,
    #[serde(default)]
    buy_filled_notional: f64,
    #[serde(default)]
    buy_fill_rate_pct: f64,
}

#[derive(Clone, Debug)]
struct DeploymentRecord {
    value: Value,
}

impl DeploymentRecord {
    fn text(&self, key: &str) -> &str {
        self.value.get(key).and_then(Value::as_str).unwrap_or("")
    }

    fn id(&self) -> &str {
        let deployment_id = self.text("deployment_id");
        if deployment_id.is_empty() {
            self.text("id")
        } else {
            deployment_id
        }
    }

    fn runtime_mode(&self) -> String {
        let mode = self.text("runtime_mode");
        if !mode.is_empty() {
            mode.to_string()
        } else if self.id().ends_with(".dryrun") {
            "dryrun".to_string()
        } else {
            "paper".to_string()
        }
    }

    fn strategy_id(&self) -> String {
        for key in ["strategy_id", "bundle_id", "strategy"] {
            let value = self.text(key);
            if !value.is_empty() {
                return value.to_string();
            }
        }
        String::new()
    }

    fn is_running(&self) -> bool {
        ["desired_state", "observed_state"]
            .iter()
            .any(|key| self.text(key).eq_ignore_ascii_case("running"))
    }

    fn is_simulated(&self) -> bool {
        matches!(
            self.text("runtime_mode").to_ascii_lowercase().as_str(),
            "dry_run" | "dryrun" | "paper"
        ) || self.id().ends_with(".dryrun")
            || self.id().ends_with(".paper")
    }
}

#[derive(Clone, Debug)]
struct LoadedReportData {
    events: Vec<EventRow>,
    daily: Vec<DailyInput>,
    pairing: DryRunPairingReport,
    order_diagnostics: Vec<OrderDiagnosticsInput>,
    runtime_evidence: DryRunRuntimeEvidence,
    deployments: Vec<DeploymentRecord>,
}

#[derive(Clone, Debug)]
struct ReportSlice {
    summary: DryRunSummary,
    metrics: DryRunMetrics,
    equity_curve: Vec<DryRunEquityPoint>,
    by_window: Vec<DryRunWindowRow>,
    daily: Vec<DryRunDailyRow>,
    daily_by_window: Vec<DryRunDailyWindowRow>,
    hourly: Vec<DryRunHourlyRow>,
    hourly_by_window: Vec<DryRunHourlyWindowRow>,
    symbols: Vec<DryRunSymbolRow>,
    symbols_by_window: Vec<DryRunSymbolRow>,
    closed_trades: Vec<DryRunClosedTradeRow>,
    recent_closed: Vec<DryRunClosedTradeRow>,
    open_positions: Vec<DryRunOpenPositionRow>,
}

#[derive(Clone, Copy)]
struct HealthSource {
    source_id: &'static str,
    table: &'static str,
    timestamp_column: &'static str,
    stale_after_seconds: u64,
    partitioned_scan: bool,
    latest_by_max: bool,
    where_sql: &'static str,
}

const HEALTH_SOURCES: [HealthSource; 11] = [
    HealthSource {
        source_id: "polymarket_quotes",
        table: "clob_quote_ticks",
        timestamp_column: "received_at",
        stale_after_seconds: 30,
        partitioned_scan: false,
        latest_by_max: true,
        where_sql: "TRUE",
    },
    HealthSource {
        source_id: "binance_lob",
        table: "binance_lob_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 30,
        partitioned_scan: true,
        latest_by_max: false,
        where_sql: "TRUE",
    },
    HealthSource {
        source_id: "binance_agg_trades",
        table: "binance_agg_trade_ticks",
        timestamp_column: "trade_time",
        stale_after_seconds: 30,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "TRUE",
    },
    HealthSource {
        source_id: "deribit_iv",
        table: "deribit_iv_ticks",
        timestamp_column: "fetched_at",
        stale_after_seconds: 300,
        partitioned_scan: true,
        latest_by_max: true,
        where_sql: "TRUE",
    },
    HealthSource {
        source_id: "deribit_atm_greeks",
        table: "deribit_atm_greeks_ticks",
        timestamp_column: "fetched_at",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "TRUE",
    },
    HealthSource {
        source_id: "binance_futures",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'binance/derivatives_snapshot'",
    },
    HealthSource {
        source_id: "binance_liquidations",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 86_400,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'binance/liquidation'",
    },
    HealthSource {
        source_id: "okx_lob",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'okx/lob'",
    },
    HealthSource {
        source_id: "bybit_lob",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'bybit/lob'",
    },
    HealthSource {
        source_id: "coinbase_lob",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'coinbase/lob'",
    },
    HealthSource {
        source_id: "kraken_lob",
        table: "cex_public_market_ticks",
        timestamp_column: "event_time",
        stale_after_seconds: 300,
        partitioned_scan: false,
        latest_by_max: false,
        where_sql: "source_key = 'kraken/lob'",
    },
];

pub fn generate_market_data_health_json() -> Result<String, String> {
    let database_url = database_url();
    let payload = run_async(async move {
        let pool = connect(&database_url).await?;
        market_data_health_payload(&pool).await
    })?;
    serde_json::to_string(&payload).map_err(|err| format!("serialize market data health: {err}"))
}

pub fn generate_dry_run_summary_json(host_root: &Path) -> Result<String, String> {
    let report = generate_dry_run_report(host_root, None, true)?;
    serde_json::to_string(&report).map_err(|err| format!("serialize dry-run summary: {err}"))
}

pub fn generate_strategy_report_html(
    host_root: &Path,
    since: Option<&str>,
) -> Result<String, String> {
    let since = parse_since(since)?;
    let report = generate_dry_run_report(host_root, since, false)?;
    Ok(render_strategy_report_html(&report, since))
}

fn generate_dry_run_report(
    host_root: &Path,
    since: Option<NaiveDate>,
    include_runtime_evidence: bool,
) -> Result<DryRunPerformanceReport, String> {
    let database_url = database_url();
    let root = host_root.to_path_buf();
    let data = run_async(async move {
        let pool = connect(&database_url).await?;
        load_report_data(&pool, &root, since, include_runtime_evidence).await
    })?;
    Ok(build_performance_report(data))
}

fn database_url() -> String {
    env::var("PLOY_DATABASE__URL")
        .or_else(|_| env::var("DATABASE_URL"))
        .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
}

fn parse_since(value: Option<&str>) -> Result<Option<NaiveDate>, String> {
    value
        .map(|value| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| "since must use YYYY-MM-DD".to_string())
        })
        .transpose()
}

fn run_async<T>(future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("create report runtime: {err}"))?;
    runtime.block_on(future)
}

async fn connect(database_url: &str) -> Result<PgPool, String> {
    timeout(
        Duration::from_secs(10),
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(database_url),
    )
    .await
    .map_err(|_| "database connection timed out after 10 seconds".to_string())?
    .map_err(|err| format!("database connection failed: {err}"))
}

async fn query_text(pool: &PgPool, query: &str) -> Result<String, String> {
    timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>(query).fetch_one(pool),
    )
    .await
    .map_err(|_| "report query timed out after 30 seconds".to_string())?
    .map_err(|err| format!("report query failed: {err}"))
}

async fn query_json<T: DeserializeOwned>(pool: &PgPool, query: &str) -> Result<T, String> {
    let raw = query_text(pool, query).await?;
    serde_json::from_str(&raw).map_err(|err| format!("report query returned invalid JSON: {err}"))
}

async fn health_query_json<T: DeserializeOwned>(pool: &PgPool, query: &str) -> Result<T, String> {
    let raw = timeout(
        HEALTH_QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>(query).fetch_one(pool),
    )
    .await
    .map_err(|_| "market data health query timed out after 10 seconds".to_string())?
    .map_err(|err| format!("market data health query failed: {err}"))?;
    serde_json::from_str(&raw)
        .map_err(|err| format!("market data health query returned invalid JSON: {err}"))
}

async fn load_report_data(
    pool: &PgPool,
    host_root: &Path,
    since: Option<NaiveDate>,
    include_runtime_evidence: bool,
) -> Result<LoadedReportData, String> {
    let events_raw = timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>(EVENTS_QUERY)
            .bind(since)
            .fetch_one(pool),
    )
    .await
    .map_err(|_| "event report query timed out after 30 seconds".to_string())?
    .map_err(|err| format!("event report query failed: {err}"))?;
    let daily_raw = timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>(DAILY_QUERY)
            .bind(since)
            .fetch_one(pool),
    )
    .await
    .map_err(|_| "daily report query timed out after 30 seconds".to_string())?
    .map_err(|err| format!("daily report query failed: {err}"))?;

    let events = serde_json::from_str(&events_raw)
        .map_err(|err| format!("event report returned invalid JSON: {err}"))?;
    let daily = serde_json::from_str(&daily_raw)
        .map_err(|err| format!("daily report returned invalid JSON: {err}"))?;
    let pairing = query_json(pool, PAIRING_QUERY).await?;
    let order_diagnostics_raw = timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>(ORDER_DIAGNOSTICS_QUERY)
            .bind(since)
            .fetch_one(pool),
    )
    .await
    .map_err(|_| "order diagnostics query timed out after 30 seconds".to_string())?
    .map_err(|err| format!("order diagnostics query failed: {err}"))?;
    let order_diagnostics = serde_json::from_str(&order_diagnostics_raw)
        .map_err(|err| format!("order diagnostics returned invalid JSON: {err}"))?;
    let runtime_evidence = if include_runtime_evidence {
        DryRunRuntimeEvidence {
            schema_version: 1,
            basis: "strategy_runtime_orders_fills_and_events".to_string(),
            events: query_json(pool, RUNTIME_EVENTS_QUERY).await?,
            orders: query_json(pool, RUNTIME_ORDERS_QUERY).await?,
            fills: query_json(pool, RUNTIME_FILLS_QUERY).await?,
        }
    } else {
        DryRunRuntimeEvidence {
            schema_version: 1,
            basis: "strategy_runtime_orders_fills_and_events".to_string(),
            events: Vec::new(),
            orders: Vec::new(),
            fills: Vec::new(),
        }
    };

    Ok(LoadedReportData {
        events,
        daily,
        pairing,
        order_diagnostics,
        runtime_evidence,
        deployments: load_deployments(host_root),
    })
}

async fn market_data_health_payload(pool: &PgPool) -> Result<Value, String> {
    let suffix = Local::now().format("%Y%m%d").to_string();
    let mut sources = Vec::with_capacity(HEALTH_SOURCES.len());
    for source in HEALTH_SOURCES {
        sources.push(source_snapshot(pool, source, &suffix).await?);
    }

    let iv_table = format!("deribit_iv_ticks_new_{suffix}");
    let iv_query = format!(
        "SELECT COALESCE(json_agg(row_to_json(row)), '[]'::json)::text FROM (\
         SELECT currency, instrument_name, mark_iv, bid_iv, ask_iv, underlying_price, fetched_at \
         FROM {iv_table} ORDER BY id DESC LIMIT 8) row"
    );
    let greeks_query = "SELECT COALESCE(json_agg(row_to_json(row)), '[]'::json)::text FROM (\
         SELECT currency, instrument_name, mark_iv, delta, gamma, vega, theta, underlying_price, fetched_at \
         FROM deribit_atm_greeks_ticks ORDER BY id DESC LIMIT 8) row";

    Ok(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "sources": sources,
        "deribit_iv_samples": health_query_json::<Value>(pool, &iv_query).await?,
        "deribit_greeks_samples": health_query_json::<Value>(pool, greeks_query).await?,
    }))
}

async fn source_snapshot(
    pool: &PgPool,
    source: HealthSource,
    suffix: &str,
) -> Result<Value, String> {
    let scan_table = if source.partitioned_scan {
        format!("{}_new_{suffix}", source.table)
    } else {
        source.table.to_string()
    };
    let latest_sql = if source.latest_by_max {
        format!(
            "SELECT max({}) AS latest_at FROM {} WHERE {}",
            source.timestamp_column, scan_table, source.where_sql
        )
    } else {
        format!(
            "SELECT {} AS latest_at FROM {} WHERE {} ORDER BY {} DESC LIMIT 1",
            source.timestamp_column, scan_table, source.where_sql, source.timestamp_column
        )
    };
    let query = format!(
        r#"
WITH table_ref AS (
  SELECT to_regclass('public.{table}') AS oid
), latest AS (
  {latest_sql}
), estimate AS (
  SELECT GREATEST(c.reltuples, 0)::bigint AS approx_rows
  FROM pg_class c JOIN table_ref r ON c.oid = r.oid
)
SELECT json_build_object(
  'source_id', '{source_id}',
  'table_name', '{table}',
  'latest_at', (SELECT latest_at FROM latest),
  'stale_after_seconds', {stale_after_seconds},
  'approx_rows', COALESCE((SELECT approx_rows FROM estimate), 0)
)::text
"#,
        table = source.table,
        latest_sql = latest_sql,
        source_id = source.source_id,
        stale_after_seconds = source.stale_after_seconds,
    );
    health_query_json(pool, &query).await
}

fn load_deployments(host_root: &Path) -> Vec<DeploymentRecord> {
    let config_dir = env_path(
        host_root,
        "PLOY_DEPLOYMENT_CONFIG_DIR",
        "config/deployments",
    );
    let state_file = env_path(
        host_root,
        "PLOY_DEPLOYMENTS_FILE",
        "data/state/deployments.json",
    );
    let status_file = env_path(
        host_root,
        "PLOY_DEPLOYMENT_STATUS_FILE",
        "run/platform/deployments.json",
    );

    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir(config_dir) {
        paths.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json")),
        );
        paths.sort();
    }
    paths.extend([state_file, status_file]);

    let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
    for path in paths {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        for record in records_from_payload(payload) {
            let wrapped = DeploymentRecord {
                value: record.clone(),
            };
            let id = wrapped.id().to_string();
            if id.is_empty() {
                continue;
            }
            let target = by_id.entry(id).or_insert_with(|| json!({}));
            if let (Some(target), Some(update)) = (target.as_object_mut(), record.as_object()) {
                for (key, value) in update {
                    if !value.is_null() && value.as_str() != Some("") {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    by_id
        .into_values()
        .map(|value| DeploymentRecord { value })
        .filter(DeploymentRecord::is_simulated)
        .collect()
}

fn env_path(host_root: &Path, key: &str, fallback: &str) -> PathBuf {
    let path = env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback));
    if path.is_absolute() {
        path
    } else {
        host_root.join(path)
    }
}

fn records_from_payload(payload: Value) -> Vec<Value> {
    match payload {
        Value::Array(values) => values
            .into_iter()
            .filter(|value| value.is_object())
            .collect(),
        Value::Object(mut object) => {
            for key in ["deployments", "items", "records"] {
                if let Some(Value::Array(values)) = object.remove(key) {
                    return values
                        .into_iter()
                        .filter(|value| value.is_object())
                        .collect();
                }
            }
            let value = Value::Object(object);
            let record = DeploymentRecord {
                value: value.clone(),
            };
            if record.id().is_empty() {
                Vec::new()
            } else {
                vec![value]
            }
        }
        _ => Vec::new(),
    }
}

fn build_performance_report(data: LoadedReportData) -> DryRunPerformanceReport {
    let all = build_report_slice(&data.events, &data.daily);
    let execution_diagnostics = build_execution_diagnostics(&data.order_diagnostics);
    let mut events_by_strategy: BTreeMap<(String, String, String), Vec<EventRow>> = BTreeMap::new();
    let mut daily_by_strategy: BTreeMap<(String, String, String), Vec<DailyInput>> =
        BTreeMap::new();
    let mut diagnostics_by_strategy: BTreeMap<(String, String, String), OrderDiagnosticsInput> =
        BTreeMap::new();

    for event in &data.events {
        events_by_strategy
            .entry(event_key(event))
            .or_default()
            .push(event.clone());
    }
    for row in &data.daily {
        daily_by_strategy
            .entry(daily_key(row))
            .or_default()
            .push(row.clone());
    }
    for row in &data.order_diagnostics {
        diagnostics_by_strategy.insert(order_key(row), row.clone());
    }

    let mut strategies = Vec::new();
    let mut represented = BTreeSet::new();

    for (key, events) in &events_by_strategy {
        let slice = build_report_slice(
            events,
            daily_by_strategy
                .get(key)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let diagnostic = diagnostics_by_strategy.get(key).cloned();
        strategies.push((
            false,
            strategy_report(
                key,
                slice,
                build_execution_diagnostics(&diagnostic.into_iter().collect::<Vec<_>>()),
            ),
        ));
        represented.insert(key.clone());
    }

    for (key, diagnostic) in &diagnostics_by_strategy {
        if represented.contains(key) {
            continue;
        }
        strategies.push((
            true,
            strategy_report(
                key,
                build_report_slice(&[], &[]),
                build_execution_diagnostics(std::slice::from_ref(diagnostic)),
            ),
        ));
        represented.insert(key.clone());
    }

    for deployment in data.deployments.iter().filter(|record| record.is_running()) {
        let key = (
            deployment.runtime_mode(),
            deployment.strategy_id(),
            deployment.id().to_string(),
        );
        if represented.contains(&key)
            || represented
                .iter()
                .any(|(_, _, deployment_id)| deployment_id == deployment.id())
        {
            continue;
        }
        let diagnostics = diagnostics_by_strategy.get(&key).cloned();
        strategies.push((
            true,
            strategy_report(
                &key,
                build_report_slice(&[], &[]),
                build_execution_diagnostics(&diagnostics.into_iter().collect::<Vec<_>>()),
            ),
        ));
        represented.insert(key);
    }

    strategies.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.deployment_id.cmp(&right.1.deployment_id))
            .then_with(|| left.1.strategy_id.cmp(&right.1.strategy_id))
    });
    let strategies = strategies
        .into_iter()
        .map(|(_, strategy)| strategy)
        .collect();

    DryRunPerformanceReport {
        generated_at: Utc::now().to_rfc3339(),
        summary: all.summary,
        metrics: all.metrics,
        equity_curve: all.equity_curve,
        by_window: all.by_window,
        daily: all.daily,
        daily_by_window: all.daily_by_window,
        hourly: all.hourly,
        hourly_by_window: all.hourly_by_window,
        symbols: all.symbols,
        symbols_by_window: all.symbols_by_window,
        closed_trades: all.closed_trades,
        recent_closed: all.recent_closed,
        open_positions: all.open_positions,
        strategies,
        pairing: data.pairing,
        execution_diagnostics: Some(execution_diagnostics),
        runtime_evidence: Some(data.runtime_evidence),
    }
}

fn event_key(row: &EventRow) -> (String, String, String) {
    (
        row.runtime_mode.clone(),
        row.strategy_id.clone(),
        row.deployment_id.clone(),
    )
}

fn daily_key(row: &DailyInput) -> (String, String, String) {
    (
        row.runtime_mode.clone(),
        row.strategy_id.clone(),
        row.deployment_id.clone(),
    )
}

fn order_key(row: &OrderDiagnosticsInput) -> (String, String, String) {
    (
        row.runtime_mode.clone(),
        row.strategy_id.clone(),
        row.deployment_id.clone(),
    )
}

fn strategy_report(
    key: &(String, String, String),
    slice: ReportSlice,
    execution_diagnostics: DryRunExecutionDiagnostics,
) -> DryRunStrategyReport {
    DryRunStrategyReport {
        runtime_mode: key.0.clone(),
        strategy_id: key.1.clone(),
        deployment_id: key.2.clone(),
        label: experiment_label(&key.0, &key.1, &key.2),
        experiment_label: Some(experiment_label(&key.0, &key.1, &key.2)),
        summary: slice.summary,
        metrics: slice.metrics,
        equity_curve: slice.equity_curve,
        by_window: slice.by_window,
        daily: slice.daily,
        daily_by_window: slice.daily_by_window,
        hourly: slice.hourly,
        hourly_by_window: slice.hourly_by_window,
        symbols: slice.symbols,
        symbols_by_window: slice.symbols_by_window,
        closed_trades: slice.closed_trades,
        recent_closed: slice.recent_closed,
        open_positions: slice.open_positions,
        execution_diagnostics: Some(execution_diagnostics),
    }
}

fn build_report_slice(events: &[EventRow], daily: &[DailyInput]) -> ReportSlice {
    let equity_curve = build_equity_curve(events);
    let mut closed_trades = build_closed_trades(events);
    let metrics = build_metrics(events, daily, &equity_curve);
    let recent_closed = closed_trades.iter().take(50).cloned().collect();
    closed_trades.truncate(250);
    ReportSlice {
        summary: build_summary(events),
        metrics,
        equity_curve,
        by_window: build_window_rows(events),
        daily: build_daily_rows(daily),
        daily_by_window: build_daily_by_window(events),
        hourly: build_hourly_rows(events),
        hourly_by_window: build_hourly_by_window(events),
        symbols: build_symbol_rows(events, false),
        symbols_by_window: build_symbol_rows(events, true),
        closed_trades,
        recent_closed,
        open_positions: build_open_positions(events),
    }
}

fn build_summary(events: &[EventRow]) -> DryRunSummary {
    let closed: Vec<_> = events.iter().filter(|event| event.is_closed).collect();
    let open: Vec<_> = events.iter().filter(|event| !event.is_closed).collect();
    let wins = closed
        .iter()
        .filter(|event| number(event.net_pnl) > 0.0)
        .count();
    DryRunSummary {
        total_trades: events.len(),
        closed_trades: closed.len(),
        wins,
        losses: closed.len().saturating_sub(wins),
        win_rate_pct: win_rate(wins, closed.len()),
        realized_pnl: rounded(closed.iter().map(|event| number(event.net_pnl)).sum(), 2),
        total_fees: rounded(closed.iter().map(|event| number(event.total_fee)).sum(), 2),
        open_positions: open.len(),
        open_exposure: rounded(open.iter().map(|event| number(event.buy_notional)).sum(), 2),
        latest_opened_at: events
            .iter()
            .filter_map(|event| event.opened_at.clone())
            .max(),
        latest_closed_at: closed
            .iter()
            .filter_map(|event| event.closed_at.clone())
            .max(),
    }
}

fn build_equity_curve(events: &[EventRow]) -> Vec<DryRunEquityPoint> {
    let mut closed: Vec<_> = events.iter().filter(|event| event.is_closed).collect();
    closed.sort_by(|left, right| {
        sort_timestamp(left.closed_at.as_deref())
            .cmp(sort_timestamp(right.closed_at.as_deref()))
            .then_with(|| left.trade_key.cmp(&right.trade_key))
    });
    let mut cumulative = 0.0_f64;
    let mut peak = 0.0_f64;
    closed
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            let pnl = number(event.net_pnl);
            cumulative += pnl;
            peak = peak.max(cumulative);
            DryRunEquityPoint {
                index: index + 1,
                label: (index + 1).to_string(),
                timestamp: event.closed_at.clone(),
                symbol: event.symbol.clone(),
                pnl: rounded(pnl, 4),
                cumulative: rounded(cumulative, 4),
                drawdown: rounded(cumulative - peak, 4),
            }
        })
        .collect()
}

fn build_metrics(
    events: &[EventRow],
    daily: &[DailyInput],
    equity_curve: &[DryRunEquityPoint],
) -> DryRunMetrics {
    let pnls: Vec<f64> = events
        .iter()
        .filter(|event| event.is_closed)
        .map(|event| number(event.net_pnl))
        .collect();
    let gross_profit: f64 = pnls.iter().copied().filter(|pnl| *pnl > 0.0).sum();
    let gross_loss: f64 = -pnls.iter().copied().filter(|pnl| *pnl < 0.0).sum::<f64>();
    let profit_factor = if gross_loss > 0.0 {
        Some(NumberOrText::Number(rounded(gross_profit / gross_loss, 4)))
    } else if gross_profit > 0.0 {
        Some(NumberOrText::Text("Infinity".to_string()))
    } else {
        None
    };
    let trade_sharpe = sharpe(&pnls, (pnls.len() as f64).sqrt());
    let daily_pnls: Vec<f64> = daily.iter().map(|row| number(row.net_pnl)).collect();
    let daily_sharpe = sharpe(&daily_pnls, 365.0_f64.sqrt());
    DryRunMetrics {
        sharpe: trade_sharpe,
        sharpe_per_trade: trade_sharpe,
        sharpe_basis: Some("closed_trade_pnl_sqrt_n".to_string()),
        closed_trade_count_for_sharpe: Some(pnls.len()),
        sharpe_daily_ann: daily_sharpe,
        daily_sharpe_basis: Some("daily_net_pnl_sqrt_365".to_string()),
        profit_factor,
        max_drawdown: equity_curve
            .iter()
            .map(|point| point.drawdown)
            .fold(0.0_f64, f64::min),
        avg_trade: (!pnls.is_empty()).then(|| rounded(mean(&pnls), 4)),
        gross_profit: rounded(gross_profit, 4),
        gross_loss: rounded(gross_loss, 4),
        equity_points: equity_curve.len(),
    }
}

fn build_window_rows(events: &[EventRow]) -> Vec<DryRunWindowRow> {
    let mut grouped: BTreeMap<Option<i64>, Vec<&EventRow>> = BTreeMap::new();
    for event in events {
        grouped.entry(event.window_secs).or_default().push(event);
    }
    let mut rows: Vec<_> = grouped
        .into_iter()
        .map(|(window_secs, window_events)| {
            let closed: Vec<_> = window_events
                .iter()
                .copied()
                .filter(|event| event.is_closed)
                .collect();
            let wins = closed
                .iter()
                .filter(|event| number(event.net_pnl) > 0.0)
                .count();
            let entries: Vec<f64> = closed
                .iter()
                .filter_map(|event| event.avg_entry_price)
                .filter(|value| value.is_finite())
                .collect();
            let ttr: Vec<i64> = window_events
                .iter()
                .filter_map(|event| event.entry_time_remaining_secs)
                .collect();
            DryRunWindowRow {
                window_secs,
                window_label: window_label(window_secs),
                total_trades: window_events.len(),
                closed_trades: closed.len(),
                wins,
                losses: closed.len().saturating_sub(wins),
                win_rate_pct: win_rate(wins, closed.len()),
                realized_pnl: rounded(closed.iter().map(|event| number(event.net_pnl)).sum(), 2),
                avg_pnl: (!closed.is_empty()).then(|| {
                    rounded(
                        mean(
                            &closed
                                .iter()
                                .map(|event| number(event.net_pnl))
                                .collect::<Vec<_>>(),
                        ),
                        2,
                    )
                }),
                avg_entry: (!entries.is_empty()).then(|| rounded(mean(&entries), 4)),
                min_entry_ttr_secs: ttr.iter().min().copied(),
                max_entry_ttr_secs: ttr.iter().max().copied(),
            }
        })
        .collect();
    rows.sort_by_key(|row| {
        (
            row.window_secs.is_none(),
            row.window_secs.unwrap_or_default(),
        )
    });
    rows
}

fn build_daily_rows(rows: &[DailyInput]) -> Vec<DryRunDailyRow> {
    let mut grouped: BTreeMap<Option<String>, Vec<&DailyInput>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.trading_day_cst.clone())
            .or_default()
            .push(row);
    }
    let mut result: Vec<_> = grouped
        .into_iter()
        .map(|(day, rows)| DryRunDailyRow {
            trading_day_cst: day,
            trade_count: sum_usize(&rows, |row| row.trade_count),
            closed_trade_count: sum_usize(&rows, |row| row.closed_trade_count),
            wins: sum_usize(&rows, |row| row.wins),
            losses: sum_usize(&rows, |row| row.losses),
            confirmed_trade_count: sum_usize(&rows, |row| row.confirmed_trade_count),
            net_pnl: rounded(rows.iter().map(|row| number(row.net_pnl)).sum(), 2),
            confirmed_pnl: rounded(
                rows.iter().map(|row| number(row.confirmed_net_pnl)).sum(),
                2,
            ),
            fees: rounded(rows.iter().map(|row| number(row.fees)).sum(), 2),
            open_quantity: rounded(rows.iter().map(|row| number(row.open_quantity)).sum(), 4),
        })
        .collect();
    result.sort_by(|left, right| right.trading_day_cst.cmp(&left.trading_day_cst));
    result
}

fn build_daily_by_window(events: &[EventRow]) -> Vec<DryRunDailyWindowRow> {
    let mut grouped: BTreeMap<(String, Option<i64>), Vec<&EventRow>> = BTreeMap::new();
    for event in events {
        if let Some(day) = day_from_event(event) {
            grouped
                .entry((day, event.window_secs))
                .or_default()
                .push(event);
        }
    }
    let mut rows: Vec<_> = grouped
        .into_iter()
        .map(|((day, window_secs), events)| {
            let closed: Vec<_> = events
                .iter()
                .copied()
                .filter(|event| event.is_closed)
                .collect();
            let wins = closed
                .iter()
                .filter(|event| number(event.net_pnl) > 0.0)
                .count();
            DryRunDailyWindowRow {
                trading_day_cst: day,
                window_secs,
                window_label: window_label(window_secs),
                trade_count: events.len(),
                closed_trade_count: closed.len(),
                wins,
                losses: closed.len().saturating_sub(wins),
                net_pnl: rounded(closed.iter().map(|event| number(event.net_pnl)).sum(), 2),
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .trading_day_cst
            .cmp(&left.trading_day_cst)
            .then_with(|| right.window_secs.cmp(&left.window_secs))
    });
    rows
}

fn build_hourly_rows(events: &[EventRow]) -> Vec<DryRunHourlyRow> {
    let mut grouped: BTreeMap<String, Vec<&EventRow>> = BTreeMap::new();
    for event in events {
        if let Some(hour) = hour_from_event(event) {
            grouped.entry(hour).or_default().push(event);
        }
    }
    let mut cumulative = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut rows = Vec::new();
    for (hour, events) in grouped {
        let closed: Vec<_> = events
            .iter()
            .copied()
            .filter(|event| event.is_closed)
            .collect();
        let wins = closed
            .iter()
            .filter(|event| number(event.net_pnl) > 0.0)
            .count();
        let net_pnl: f64 = closed.iter().map(|event| number(event.net_pnl)).sum();
        cumulative += net_pnl;
        peak = peak.max(cumulative);
        rows.push(DryRunHourlyRow {
            trading_hour_cst: hour,
            trade_count: events.len(),
            closed_trade_count: closed.len(),
            wins,
            losses: closed.len().saturating_sub(wins),
            net_pnl: rounded(net_pnl, 2),
            cumulative_pnl: rounded(cumulative, 4),
            drawdown: rounded(cumulative - peak, 4),
        });
    }
    rows.reverse();
    rows
}

fn build_hourly_by_window(events: &[EventRow]) -> Vec<DryRunHourlyWindowRow> {
    let mut grouped: BTreeMap<(String, Option<i64>), Vec<&EventRow>> = BTreeMap::new();
    for event in events {
        if let Some(hour) = hour_from_event(event) {
            grouped
                .entry((hour, event.window_secs))
                .or_default()
                .push(event);
        }
    }
    let mut rows: Vec<_> = grouped
        .into_iter()
        .map(|((hour, window_secs), events)| {
            let closed: Vec<_> = events
                .iter()
                .copied()
                .filter(|event| event.is_closed)
                .collect();
            let wins = closed
                .iter()
                .filter(|event| number(event.net_pnl) > 0.0)
                .count();
            DryRunHourlyWindowRow {
                trading_hour_cst: hour,
                window_secs,
                window_label: window_label(window_secs),
                trade_count: events.len(),
                closed_trade_count: closed.len(),
                wins,
                losses: closed.len().saturating_sub(wins),
                net_pnl: rounded(closed.iter().map(|event| number(event.net_pnl)).sum(), 2),
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .trading_hour_cst
            .cmp(&left.trading_hour_cst)
            .then_with(|| right.window_secs.cmp(&left.window_secs))
    });
    rows
}

fn build_symbol_rows(events: &[EventRow], include_window: bool) -> Vec<DryRunSymbolRow> {
    let mut grouped: BTreeMap<(String, Option<i64>), Vec<&EventRow>> = BTreeMap::new();
    for event in events {
        grouped
            .entry((
                event
                    .symbol
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                include_window.then_some(event.window_secs).flatten(),
            ))
            .or_default()
            .push(event);
    }
    let mut rows: Vec<_> = grouped
        .into_iter()
        .map(|((symbol, window_secs), events)| {
            let closed: Vec<_> = events
                .iter()
                .copied()
                .filter(|event| event.is_closed)
                .collect();
            let entries: Vec<_> = closed
                .iter()
                .filter_map(|event| event.avg_entry_price)
                .filter(|value| value.is_finite())
                .collect();
            let wins = closed
                .iter()
                .filter(|event| number(event.net_pnl) > 0.0)
                .count();
            DryRunSymbolRow {
                symbol,
                trades: events.len(),
                wins,
                losses: closed.len().saturating_sub(wins),
                net_pnl: rounded(closed.iter().map(|event| number(event.net_pnl)).sum(), 2),
                avg_entry: (!entries.is_empty()).then(|| rounded(mean(&entries), 4)),
                window_secs,
                window_label: include_window.then(|| window_label(window_secs)),
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .net_pnl
            .total_cmp(&left.net_pnl)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
    rows
}

fn build_closed_trades(events: &[EventRow]) -> Vec<DryRunClosedTradeRow> {
    let mut closed: Vec<_> = events.iter().filter(|event| event.is_closed).collect();
    closed.sort_by(|left, right| {
        sort_timestamp(right.closed_at.as_deref())
            .cmp(sort_timestamp(left.closed_at.as_deref()))
            .then_with(|| right.trade_key.cmp(&left.trade_key))
    });
    closed.into_iter().map(closed_trade_row).collect()
}

fn closed_trade_row(event: &EventRow) -> DryRunClosedTradeRow {
    DryRunClosedTradeRow {
        runtime_mode: optional_text(&event.runtime_mode),
        strategy_id: optional_text(&event.strategy_id),
        deployment_id: optional_text(&event.deployment_id),
        experiment_label: Some(experiment_label(
            &event.runtime_mode,
            &event.strategy_id,
            &event.deployment_id,
        )),
        trade_key: event.trade_key.clone(),
        event_id: event.event_id.clone(),
        symbol: event.symbol.clone(),
        window_secs: event.window_secs,
        window_label: window_label(event.window_secs),
        market_side: event.market_side.clone(),
        entry_price: event.avg_entry_price.map(|value| rounded(value, 4)),
        exit_price: event.avg_exit_price.map(|value| rounded(value, 4)),
        exit_type: exit_type(event.avg_exit_price),
        quantity: rounded(number(event.buy_quantity), 4),
        notional: rounded(number(event.buy_notional), 4),
        net_pnl: rounded(number(event.net_pnl), 4),
        entry_time_remaining_secs: event.entry_time_remaining_secs,
        opened_at: event.opened_at.clone(),
        closed_at: event.closed_at.clone(),
    }
}

fn build_open_positions(events: &[EventRow]) -> Vec<DryRunOpenPositionRow> {
    let mut open: Vec<_> = events.iter().filter(|event| !event.is_closed).collect();
    open.sort_by(|left, right| {
        sort_timestamp(right.opened_at.as_deref())
            .cmp(sort_timestamp(left.opened_at.as_deref()))
            .then_with(|| right.trade_key.cmp(&left.trade_key))
    });
    open.into_iter()
        .map(|event| DryRunOpenPositionRow {
            runtime_mode: optional_text(&event.runtime_mode),
            strategy_id: optional_text(&event.strategy_id),
            deployment_id: optional_text(&event.deployment_id),
            experiment_label: Some(experiment_label(
                &event.runtime_mode,
                &event.strategy_id,
                &event.deployment_id,
            )),
            trade_key: event.trade_key.clone(),
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            window_secs: event.window_secs,
            window_label: window_label(event.window_secs),
            market_side: event.market_side.clone(),
            entry_price: event.avg_entry_price.map(|value| rounded(value, 4)),
            quantity: rounded(number(event.buy_quantity), 4),
            notional: rounded(number(event.buy_notional), 4),
            entry_time_remaining_secs: event.entry_time_remaining_secs,
            opened_at: event.opened_at.clone(),
        })
        .collect()
}

fn build_execution_diagnostics(rows: &[OrderDiagnosticsInput]) -> DryRunExecutionDiagnostics {
    let total_orders: f64 = rows.iter().map(|row| row.total_orders).sum();
    let buy_orders: f64 = rows.iter().map(|row| row.buy_orders).sum();
    let sell_orders: f64 = rows.iter().map(|row| row.sell_orders).sum();
    let rejected_orders: f64 = rows.iter().map(|row| row.rejected_orders).sum();
    let rejected_buy_orders: f64 = rows.iter().map(|row| row.rejected_buy_orders).sum();
    let partial_buy_orders: f64 = rows.iter().map(|row| row.partial_buy_orders).sum();
    let requested: f64 = rows.iter().map(|row| row.buy_requested_notional).sum();
    let filled: f64 = rows.iter().map(|row| row.buy_filled_notional).sum();
    let buy_fill_rate = if requested > 0.0 {
        rounded(filled / requested * 100.0, 2)
    } else {
        0.0
    };
    let rejected_buy_rate = if buy_orders > 0.0 {
        rounded(rejected_buy_orders / buy_orders * 100.0, 2)
    } else {
        0.0
    };
    let summary = BTreeMap::from([
        ("total_orders".to_string(), json!(total_orders as usize)),
        ("buy_orders".to_string(), json!(buy_orders as usize)),
        ("sell_orders".to_string(), json!(sell_orders as usize)),
        (
            "rejected_orders".to_string(),
            json!(rejected_orders as usize),
        ),
        (
            "rejected_buy_orders".to_string(),
            json!(rejected_buy_orders as usize),
        ),
        (
            "partial_buy_orders".to_string(),
            json!(partial_buy_orders as usize),
        ),
        (
            "buy_requested_notional".to_string(),
            json!(rounded(requested, 4)),
        ),
        ("buy_filled_notional".to_string(), json!(rounded(filled, 4))),
        ("buy_fill_rate_pct".to_string(), json!(buy_fill_rate)),
        (
            "rejected_buy_rate_pct".to_string(),
            json!(rejected_buy_rate),
        ),
    ]);
    DryRunExecutionDiagnostics {
        basis: "strategy_runtime_orders".to_string(),
        partial_buy_threshold_pct: 98,
        summary,
        strategies: rows
            .iter()
            .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
            .collect(),
    }
}

fn experiment_label(runtime_mode: &str, strategy_id: &str, deployment_id: &str) -> String {
    let known = match deployment_id {
        "pm5d.threelayer.dryrun" => Some("TL v1 Base EVCal"),
        "pm5d.threelayer.champion.dryrun" => Some("TL v2 Champion EVCal"),
        "pm5d.threelayer.obi-soft.dryrun" => Some("TL v3 OBI-soft EVCal"),
        "pm5d.threelayer.obi-hard.dryrun" => Some("TL v4 OBI-hard EVCal"),
        "pm5d.threelayer.continuation-soft.dryrun" => Some("TL v5 Continuation-soft EVCal"),
        "pm5d.threelayer.settlement-probability-btc-eth.dryrun" => {
            Some("TL Settlement Probability BTC/ETH")
        }
        _ => None,
    };
    if let Some(label) = known {
        return label.to_string();
    }
    if let Some(variant) = deployment_id
        .strip_prefix("pm5d.threelayer.")
        .and_then(|value| value.strip_suffix(".dryrun"))
    {
        return format!(
            "TL {}",
            title_case(if variant.is_empty() { "base" } else { variant })
        );
    }
    if let Some(variant) = deployment_id
        .strip_prefix("pm5d.")
        .and_then(|value| value.strip_suffix(".dryrun"))
    {
        return format!("PM5D {}", title_case(variant));
    }
    if !deployment_id.is_empty() {
        deployment_id.to_string()
    } else if !strategy_id.is_empty() {
        strategy_id.to_string()
    } else if !runtime_mode.is_empty() {
        runtime_mode.to_string()
    } else {
        "unknown".to_string()
    }
}

fn title_case(value: &str) -> String {
    value
        .split(['-', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn day_from_event(event: &EventRow) -> Option<String> {
    let timestamp = event
        .closed_at
        .as_deref()
        .or(event.last_fill_at.as_deref())
        .or(event.opened_at.as_deref())?;
    if let Some(parsed) = parse_timestamp(timestamp) {
        let cst = FixedOffset::east_opt(8 * 60 * 60).expect("valid CST offset");
        Some(parsed.with_timezone(&cst).date_naive().to_string())
    } else {
        timestamp.get(..10).map(str::to_string)
    }
}

fn hour_from_event(event: &EventRow) -> Option<String> {
    let timestamp = event
        .closed_at
        .as_deref()
        .or(event.last_fill_at.as_deref())
        .or(event.opened_at.as_deref())?;
    if let Some(parsed) = parse_timestamp(timestamp) {
        let cst = FixedOffset::east_opt(8 * 60 * 60).expect("valid CST offset");
        Some(
            parsed
                .with_timezone(&cst)
                .format("%Y-%m-%dT%H:00:00+08:00")
                .to_string(),
        )
    } else {
        timestamp.get(..13).map(|prefix| format!("{prefix}:00"))
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(&value.replace(' ', "T"))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f"))
                .ok()
                .and_then(|naive| {
                    FixedOffset::east_opt(0)?
                        .from_local_datetime(&naive)
                        .single()
                })
        })
}

fn exit_type(exit_price: Option<f64>) -> String {
    match exit_price.filter(|value| value.is_finite()) {
        Some(value) if value >= 0.99 => "WIN".to_string(),
        Some(value) if value <= 0.01 => "LOSS".to_string(),
        _ => "TP/SL".to_string(),
    }
}

fn window_label(window_secs: Option<i64>) -> String {
    match window_secs {
        Some(300) => "5m".to_string(),
        Some(900) => "15m".to_string(),
        Some(seconds) => format!("{seconds}s"),
        None => "unknown".to_string(),
    }
}

fn optional_text(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn sort_timestamp(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

fn number(value: Option<f64>) -> f64 {
    value.filter(|value| value.is_finite()).unwrap_or_default()
}

fn rounded(value: f64, digits: i32) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let factor = 10_f64.powi(digits);
    (value * factor).round() / factor
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn sharpe(values: &[f64], scale: f64) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let mean = mean(values);
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    let sigma = variance.sqrt();
    (sigma > 0.0).then(|| rounded(mean / sigma * scale, 4))
}

fn win_rate(wins: usize, closed: usize) -> f64 {
    if closed == 0 {
        0.0
    } else {
        rounded(wins as f64 / closed as f64 * 100.0, 1)
    }
}

fn sum_usize(rows: &[&DailyInput], value: impl Fn(&DailyInput) -> Option<f64>) -> usize {
    rows.iter().map(|row| number(value(row))).sum::<f64>() as usize
}

pub fn render_strategy_report_html(
    report: &DryRunPerformanceReport,
    since: Option<NaiveDate>,
) -> String {
    let diagnostics = report.execution_diagnostics.as_ref();
    let rejected_buy_orders = diagnostics_value(diagnostics, "rejected_buy_orders");
    let partial_buy_orders = diagnostics_value(diagnostics, "partial_buy_orders");
    let requested_notional = diagnostics_value(diagnostics, "buy_requested_notional");
    let filled_notional = diagnostics_value(diagnostics, "buy_filled_notional");
    let rejected_buy_rate = diagnostics_value(diagnostics, "rejected_buy_rate_pct");
    let fill_rate = diagnostics_value(diagnostics, "buy_fill_rate_pct");
    let since_label = since
        .map(|date| format!(" (since {date})"))
        .unwrap_or_default();

    let symbol_rows = report
        .symbols
        .iter()
        .map(|row| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>${:.2}</td><td>{}</td></tr>",
                escape_html(&row.symbol),
                row.trades,
                row.wins,
                row.losses,
                row.net_pnl,
                row.avg_entry
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "—".to_string()),
            )
        })
        .collect::<String>();
    let trade_rows = report
        .recent_closed
        .iter()
        .map(|row| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>${:.2}</td><td>{}</td><td>{}</td></tr>",
                escape_html(row.symbol.as_deref().unwrap_or("unknown")),
                escape_html(row.market_side.as_deref().unwrap_or("unknown")),
                row.entry_price
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "—".to_string()),
                row.exit_price
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "—".to_string()),
                escape_html(&row.exit_type),
                row.net_pnl,
                escape_html(row.opened_at.as_deref().unwrap_or("—")),
                escape_html(row.closed_at.as_deref().unwrap_or("—")),
            )
        })
        .collect::<String>();
    let open_rows = report
        .open_positions
        .iter()
        .map(|row| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>${:.2}</td><td>{:.2}</td><td>{}</td></tr>",
                escape_html(row.symbol.as_deref().unwrap_or("unknown")),
                escape_html(row.market_side.as_deref().unwrap_or("unknown")),
                row.entry_price
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "—".to_string()),
                row.notional,
                row.quantity,
                escape_html(row.opened_at.as_deref().unwrap_or("—")),
            )
        })
        .collect::<String>();

    let equity_labels = json_for_script(
        report
            .equity_curve
            .iter()
            .map(|point| {
                point
                    .timestamp
                    .clone()
                    .unwrap_or_else(|| point.label.clone())
            })
            .collect::<Vec<_>>(),
    );
    let equity_values = json_for_script(
        report
            .equity_curve
            .iter()
            .map(|point| point.cumulative)
            .collect::<Vec<_>>(),
    );
    let daily_labels = json_for_script(
        report
            .daily
            .iter()
            .rev()
            .map(|row| {
                row.trading_day_cst
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            })
            .collect::<Vec<_>>(),
    );
    let daily_values = json_for_script(
        report
            .daily
            .iter()
            .rev()
            .map(|row| row.net_pnl)
            .collect::<Vec<_>>(),
    );

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Three-Layer Strategy Report{since_label}</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
<style>
* {{ box-sizing: border-box; }}
body {{ margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #0f172a; color: #e2e8f0; padding: 24px; }}
.container {{ max-width: 1200px; margin: 0 auto; }}
.subtitle {{ color: #94a3b8; }}
.cards {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; margin: 24px 0; }}
.card, .section {{ background: #1e293b; border-radius: 8px; padding: 16px; }}
.section {{ margin: 16px 0; overflow-x: auto; }}
.label {{ color: #94a3b8; font-size: .75rem; text-transform: uppercase; }}
.value {{ font-size: 1.35rem; font-weight: 700; margin-top: 4px; }}
table {{ width: 100%; border-collapse: collapse; font-size: .8rem; }}
th, td {{ text-align: left; padding: 8px; border-bottom: 1px solid #334155; }}
th {{ color: #94a3b8; text-transform: uppercase; font-size: .7rem; }}
.charts {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 16px; }}
</style>
</head>
<body><main class="container">
<h1>Three-Layer Scoring Strategy</h1>
<p class="subtitle">Dry-run report{since_label} &middot; Generated {generated}</p>
<section class="cards">
<div class="card"><div class="label">Net PnL</div><div class="value">${realized_pnl:.2}</div></div>
<div class="card"><div class="label">Trades</div><div class="value">{trades}</div></div>
<div class="card"><div class="label">Win Rate</div><div class="value">{win_rate:.1}%</div></div>
<div class="card"><div class="label">Sharpe / Trade</div><div class="value">{trade_sharpe}</div></div>
<div class="card"><div class="label">Sharpe Daily Ann</div><div class="value">{daily_sharpe}</div></div>
<div class="card"><div class="label">Max Drawdown</div><div class="value">${max_drawdown:.2}</div></div>
<div class="card"><div class="label">Avg PnL/Trade</div><div class="value">{avg_trade}</div></div>
<div class="card"><div class="label">Total Fees</div><div class="value">${total_fees:.2}</div></div>
<div class="card"><div class="label">Open Positions</div><div class="value">{open_positions}</div></div>
<div class="card"><div class="label">Requested BUY</div><div class="value">${requested_notional:.2}</div></div>
<div class="card"><div class="label">Filled BUY</div><div class="value">${filled_notional:.2}</div></div>
<div class="card"><div class="label">BUY Fill Rate</div><div class="value">{fill_rate:.1}%</div></div>
<div class="card"><div class="label">Partial Fills</div><div class="value">{partial_buy_orders:.0}</div></div>
<div class="card"><div class="label">Rejected BUY</div><div class="value">{rejected_buy_orders:.0}</div></div>
<div class="card"><div class="label">BUY Reject Rate</div><div class="value">{rejected_buy_rate:.1}%</div></div>
</section>
<section class="charts">
<div class="section"><h2>Cumulative PnL</h2><canvas id="equity"></canvas></div>
<div class="section"><h2>Daily PnL</h2><canvas id="daily"></canvas></div>
</section>
<section class="section"><h2>Performance by Symbol</h2><table><thead><tr><th>Symbol</th><th>Trades</th><th>Wins</th><th>Losses</th><th>Net PnL</th><th>Avg Entry</th></tr></thead><tbody>{symbol_rows}</tbody></table></section>
<section class="section"><h2>Recent Closed Trades</h2><table><thead><tr><th>Symbol</th><th>Side</th><th>Entry</th><th>Exit</th><th>Exit Type</th><th>PnL</th><th>Opened</th><th>Closed</th></tr></thead><tbody>{trade_rows}</tbody></table></section>
<section class="section"><h2>Open Positions</h2><table><thead><tr><th>Symbol</th><th>Side</th><th>Entry</th><th>Notional</th><th>Quantity</th><th>Opened</th></tr></thead><tbody>{open_rows}</tbody></table></section>
</main>
<script>
new Chart(document.getElementById('equity'), {{type:'line',data:{{labels:{equity_labels},datasets:[{{label:'Cumulative PnL',data:{equity_values},borderColor:'#22c55e',tension:.2}}]}}}});
new Chart(document.getElementById('daily'), {{type:'bar',data:{{labels:{daily_labels},datasets:[{{label:'Daily PnL',data:{daily_values},backgroundColor:'#38bdf8'}}]}}}});
</script>
</body></html>"#,
        generated = escape_html(&report.generated_at),
        realized_pnl = report.summary.realized_pnl,
        trades = report.summary.closed_trades,
        win_rate = report.summary.win_rate_pct,
        trade_sharpe = option_metric(report.metrics.sharpe_per_trade),
        daily_sharpe = option_metric(report.metrics.sharpe_daily_ann),
        max_drawdown = report.metrics.max_drawdown.abs(),
        avg_trade = report
            .metrics
            .avg_trade
            .map(|value| format!("${value:.2}"))
            .unwrap_or_else(|| "—".to_string()),
        total_fees = report.summary.total_fees,
        open_positions = report.summary.open_positions,
    )
}

fn diagnostics_value(diagnostics: Option<&DryRunExecutionDiagnostics>, key: &str) -> f64 {
    diagnostics
        .and_then(|diagnostics| diagnostics.summary.get(key))
        .and_then(Value::as_f64)
        .unwrap_or_default()
}

fn option_metric(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "—".to_string())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn json_for_script(value: impl serde::Serialize) -> String {
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "[]".to_string())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(pnl: f64, closed_at: &str, window_secs: i64, trade_key: &str) -> EventRow {
        EventRow {
            runtime_mode: "dry_run".to_string(),
            strategy_id: "three_layer".to_string(),
            deployment_id: "pm5d.threelayer.obi-hard.dryrun".to_string(),
            trade_key: Some(trade_key.to_string()),
            event_id: Some(format!("event-{trade_key}")),
            symbol: Some("BTC-5m".to_string()),
            market_side: Some("YES".to_string()),
            opened_at: Some("2026-04-28T17:00:00+00:00".to_string()),
            closed_at: Some(closed_at.to_string()),
            buy_quantity: Some(10.0),
            buy_notional: Some(5.0),
            total_fee: Some(0.02),
            avg_entry_price: Some(0.5),
            avg_exit_price: Some(if pnl > 0.0 { 1.0 } else { 0.0 }),
            net_pnl: Some(pnl),
            is_closed: true,
            window_secs: Some(window_secs),
            entry_time_remaining_secs: Some(120),
            ..EventRow::default()
        }
    }

    fn pairing() -> DryRunPairingReport {
        DryRunPairingReport {
            pair_key: "runtime_mode,strategy_id,deployment_id,event_id".to_string(),
            mixed_event_groups: 0,
            fills_in_mixed_event_groups: 0,
            current_view_rows: 0,
            side_aware_rows: 0,
        }
    }

    fn evidence() -> DryRunRuntimeEvidence {
        DryRunRuntimeEvidence {
            schema_version: 1,
            basis: "strategy_runtime_orders_fills_and_events".to_string(),
            events: Vec::new(),
            orders: Vec::new(),
            fills: Vec::new(),
        }
    }

    #[test]
    fn summary_fixture_preserves_python_aggregation_and_sharpe_basis() {
        let mut events = vec![
            event(1.0, "2026-04-29T01:00:00+00:00", 300, "a"),
            event(-0.5, "2026-04-29T02:00:00+00:00", 300, "b"),
            event(1.5, "2026-04-29T03:00:00+00:00", 900, "c"),
        ];
        events.push(EventRow {
            runtime_mode: "dry_run".to_string(),
            strategy_id: "three_layer".to_string(),
            deployment_id: "pm5d.threelayer.obi-hard.dryrun".to_string(),
            opened_at: Some("2026-04-29T04:00:00+00:00".to_string()),
            buy_notional: Some(12.5),
            ..EventRow::default()
        });
        let daily = vec![
            DailyInput {
                net_pnl: Some(1.0),
                ..DailyInput::default()
            },
            DailyInput {
                net_pnl: Some(-0.5),
                ..DailyInput::default()
            },
        ];

        let slice = build_report_slice(&events, &daily);
        let expected = ((1.0 - 0.5 + 1.5) / 3.0) / (13.0_f64 / 12.0).sqrt() * 3.0_f64.sqrt();

        assert_eq!(slice.summary.total_trades, 4);
        assert_eq!(slice.summary.closed_trades, 3);
        assert_eq!(slice.summary.wins, 2);
        assert_eq!(slice.summary.losses, 1);
        assert_eq!(slice.summary.open_positions, 1);
        assert_eq!(slice.summary.open_exposure, 12.5);
        assert_eq!(
            slice.metrics.sharpe_basis.as_deref(),
            Some("closed_trade_pnl_sqrt_n")
        );
        assert_eq!(
            slice.metrics.daily_sharpe_basis.as_deref(),
            Some("daily_net_pnl_sqrt_365")
        );
        assert!((slice.metrics.sharpe_per_trade.unwrap() - expected).abs() < 0.0001);
    }

    #[test]
    fn cst_window_fixture_preserves_day_hour_order_and_drawdown() {
        let events = vec![
            event(5.0, "2026-04-28T17:30:00+00:00", 300, "a"),
            event(-8.0, "2026-04-28T18:15:00+00:00", 900, "b"),
        ];

        let slice = build_report_slice(&events, &[]);

        assert_eq!(slice.daily_by_window[0].trading_day_cst, "2026-04-29");
        assert_eq!(
            slice.hourly[0].trading_hour_cst,
            "2026-04-29T02:00:00+08:00"
        );
        assert_eq!(slice.hourly[0].net_pnl, -8.0);
        assert_eq!(slice.hourly[0].drawdown, -8.0);
        assert_eq!(
            slice.hourly[1].trading_hour_cst,
            "2026-04-29T01:00:00+08:00"
        );
        assert_eq!(slice.hourly_by_window[0].window_label, "15m");
    }

    #[test]
    fn health_contract_keeps_all_sources_and_thresholds() {
        let by_id: BTreeMap<_, _> = HEALTH_SOURCES
            .iter()
            .map(|source| (source.source_id, source.stale_after_seconds))
            .collect();

        assert_eq!(by_id.len(), 11);
        assert_eq!(by_id["polymarket_quotes"], 30);
        assert_eq!(by_id["binance_liquidations"], 86_400);
        for source in ["okx_lob", "bybit_lob", "coinbase_lob", "kraken_lob"] {
            assert_eq!(by_id[source], 300);
        }
    }

    #[test]
    fn report_queries_keep_metadata_bridge_and_runtime_evidence_sources() {
        let settlement = EVENTS_QUERY
            .find("LEFT JOIN pm_token_settlements s ON s.token_id = t.token_id")
            .expect("token settlement bridge");
        let settlement_metadata = EVENTS_QUERY
            .find("LEFT JOIN pm_market_metadata ms ON ms.market_slug = s.market_slug")
            .expect("settlement metadata");
        let event_metadata = EVENTS_QUERY
            .find("LEFT JOIN pm_market_metadata me ON me.market_slug = t.event_id")
            .expect("event metadata fallback");

        assert!(settlement < settlement_metadata);
        assert!(settlement_metadata < event_metadata);
        assert!(EVENTS_QUERY
            .contains("COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug)"));
        assert!(
            RUNTIME_EVENTS_QUERY.contains("LEFT JOIN strategy_runtime_event_track_record track")
        );
        assert!(RUNTIME_EVENTS_QUERY.contains("'signal_inputs'"));
        assert!(RUNTIME_ORDERS_QUERY.contains("'context', o.context"));
        assert!(RUNTIME_FILLS_QUERY.contains("FROM strategy_runtime_fills f"));
    }

    #[test]
    fn running_deployment_without_trades_is_kept_as_zero_activity_strategy() {
        let root = std::env::temp_dir().join(format!(
            "ploy-rust-report-deployment-{}",
            std::process::id()
        ));
        let config_dir = root.join("config/deployments");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&config_dir).expect("create deployment fixture directory");
        fs::write(
            config_dir.join("candidate.json"),
            serde_json::to_vec(&json!({
                "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                "bundle_id": "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun",
                "runtime_mode": "paper",
                "account_id": "acct-pm5d-dryrun",
                "desired_state": "running"
            }))
            .unwrap(),
        )
        .expect("write deployment fixture");

        let deployments = load_deployments(&root);
        assert_eq!(deployments.len(), 1);
        assert!(deployments[0].is_running());
        let report = build_performance_report(LoadedReportData {
            events: Vec::new(),
            daily: Vec::new(),
            pairing: pairing(),
            order_diagnostics: Vec::new(),
            runtime_evidence: evidence(),
            deployments,
        });

        assert_eq!(report.strategies.len(), 1);
        assert_eq!(
            report.strategies[0].deployment_id,
            "pm5d.threelayer.settlement-probability-btc-eth.dryrun"
        );
        assert_eq!(report.strategies[0].summary.total_trades, 0);
        assert_eq!(
            report.strategies[0]
                .execution_diagnostics
                .as_ref()
                .unwrap()
                .basis,
            "strategy_runtime_orders"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dry_run_payload_matches_operator_contract_and_order_diagnostics() {
        let data = LoadedReportData {
            events: vec![event(2.0, "2026-04-29T01:00:00+00:00", 300, "a")],
            daily: Vec::new(),
            pairing: pairing(),
            order_diagnostics: vec![OrderDiagnosticsInput {
                runtime_mode: "dry_run".to_string(),
                strategy_id: "three_layer".to_string(),
                deployment_id: "pm5d.threelayer.obi-hard.dryrun".to_string(),
                total_orders: 3.0,
                buy_orders: 2.0,
                sell_orders: 1.0,
                rejected_orders: 1.0,
                rejected_buy_orders: 1.0,
                partial_buy_orders: 1.0,
                buy_requested_notional: 30.0,
                buy_filled_notional: 15.0,
                buy_fill_rate_pct: 50.0,
            }],
            runtime_evidence: evidence(),
            deployments: Vec::new(),
        };

        let report = build_performance_report(data);
        let raw = serde_json::to_string(&report).expect("serialize report");
        let parsed: DryRunPerformanceReport =
            serde_json::from_str(&raw).expect("operator contract accepts report");
        let diagnostics = parsed.execution_diagnostics.expect("diagnostics");

        assert_eq!(diagnostics.basis, "strategy_runtime_orders");
        assert_eq!(diagnostics.summary["rejected_buy_orders"], 1);
        assert_eq!(diagnostics.summary["buy_fill_rate_pct"], 50.0);
        assert_eq!(parsed.strategies[0].label, "TL v4 OBI-hard EVCal");
    }

    #[test]
    fn strategy_html_keeps_key_fields_and_escapes_market_values() {
        let mut malicious = event(2.0, "2026-04-29T01:00:00+00:00", 300, "a");
        malicious.symbol = Some("<script>alert(1)</script>".to_string());
        let report = build_performance_report(LoadedReportData {
            events: vec![malicious],
            daily: Vec::new(),
            pairing: pairing(),
            order_diagnostics: vec![OrderDiagnosticsInput {
                buy_orders: 2.0,
                rejected_buy_orders: 1.0,
                buy_requested_notional: 20.0,
                buy_filled_notional: 10.0,
                ..OrderDiagnosticsInput::default()
            }],
            runtime_evidence: evidence(),
            deployments: Vec::new(),
        });

        let html = render_strategy_report_html(
            &report,
            Some(NaiveDate::from_ymd_opt(2026, 4, 29).unwrap()),
        );

        for field in [
            "Sharpe / Trade",
            "Sharpe Daily Ann",
            "Requested BUY",
            "Filled BUY",
            "Rejected BUY",
            "BUY Reject Rate",
            "Recent Closed Trades",
            "since 2026-04-29",
        ] {
            assert!(html.contains(field), "missing HTML field: {field}");
        }
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    #[test]
    fn invalid_since_is_rejected_before_database_access() {
        assert_eq!(
            parse_since(Some("2026-02-30")).unwrap_err(),
            "since must use YYYY-MM-DD"
        );
    }

    #[test]
    fn strategy_label_keeps_versioned_and_humanized_variants() {
        assert_eq!(
            experiment_label("dry_run", "three_layer", "pm5d.threelayer.obi-hard.dryrun"),
            "TL v4 OBI-hard EVCal"
        );
        assert_eq!(
            experiment_label(
                "dry_run",
                "three_layer",
                "pm5d.threelayer.some-new-gate.dryrun"
            ),
            "TL Some New Gate"
        );
    }
}
