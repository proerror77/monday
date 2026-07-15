use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunSummary {
    pub total_trades: usize,
    pub closed_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: f64,
    pub realized_pnl: f64,
    pub total_fees: f64,
    pub open_positions: usize,
    pub open_exposure: f64,
    pub latest_opened_at: Option<String>,
    pub latest_closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NumberOrText {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunMetrics {
    pub sharpe: Option<f64>,
    #[serde(default)]
    pub sharpe_per_trade: Option<f64>,
    #[serde(default)]
    pub sharpe_basis: Option<String>,
    #[serde(default)]
    pub closed_trade_count_for_sharpe: Option<usize>,
    #[serde(default)]
    pub sharpe_daily_ann: Option<f64>,
    #[serde(default)]
    pub daily_sharpe_basis: Option<String>,
    pub profit_factor: Option<NumberOrText>,
    pub max_drawdown: f64,
    pub avg_trade: Option<f64>,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub equity_points: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunEquityPoint {
    pub index: usize,
    pub label: String,
    pub timestamp: Option<String>,
    pub symbol: Option<String>,
    pub pnl: f64,
    pub cumulative: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunWindowRow {
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub total_trades: usize,
    pub closed_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: f64,
    pub realized_pnl: f64,
    pub avg_pnl: Option<f64>,
    pub avg_entry: Option<f64>,
    pub min_entry_ttr_secs: Option<i64>,
    pub max_entry_ttr_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunDailyRow {
    pub trading_day_cst: Option<String>,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub confirmed_trade_count: usize,
    pub net_pnl: f64,
    pub confirmed_pnl: f64,
    pub fees: f64,
    pub open_quantity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunDailyWindowRow {
    pub trading_day_cst: String,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunHourlyRow {
    pub trading_hour_cst: String,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
    pub cumulative_pnl: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunHourlyWindowRow {
    pub trading_hour_cst: String,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub trade_count: usize,
    pub closed_trade_count: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunSymbolRow {
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_pnl: f64,
    pub avg_entry: Option<f64>,
    pub window_secs: Option<i64>,
    pub window_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunClosedTradeRow {
    pub runtime_mode: Option<String>,
    pub strategy_id: Option<String>,
    pub deployment_id: Option<String>,
    pub experiment_label: Option<String>,
    pub trade_key: Option<String>,
    pub event_id: Option<String>,
    pub symbol: Option<String>,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub market_side: Option<String>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub exit_type: String,
    pub quantity: f64,
    pub notional: f64,
    pub net_pnl: f64,
    pub entry_time_remaining_secs: Option<i64>,
    pub opened_at: Option<String>,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunOpenPositionRow {
    pub runtime_mode: Option<String>,
    pub strategy_id: Option<String>,
    pub deployment_id: Option<String>,
    pub experiment_label: Option<String>,
    pub trade_key: Option<String>,
    pub event_id: Option<String>,
    pub symbol: Option<String>,
    pub window_secs: Option<i64>,
    pub window_label: String,
    pub market_side: Option<String>,
    pub entry_price: Option<f64>,
    pub quantity: f64,
    pub notional: f64,
    pub entry_time_remaining_secs: Option<i64>,
    pub opened_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunPairingReport {
    pub pair_key: String,
    pub mixed_event_groups: usize,
    pub fills_in_mixed_event_groups: usize,
    pub current_view_rows: usize,
    pub side_aware_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunExecutionDiagnostics {
    pub basis: String,
    pub partial_buy_threshold_pct: usize,
    pub summary: BTreeMap<String, serde_json::Value>,
    pub strategies: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunRuntimeEvidence {
    pub schema_version: u32,
    pub basis: String,
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    #[serde(default)]
    pub orders: Vec<serde_json::Value>,
    #[serde(default)]
    pub fills: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunStrategyReport {
    pub runtime_mode: String,
    pub strategy_id: String,
    pub deployment_id: String,
    pub label: String,
    pub experiment_label: Option<String>,
    pub summary: DryRunSummary,
    pub metrics: DryRunMetrics,
    pub equity_curve: Vec<DryRunEquityPoint>,
    pub by_window: Vec<DryRunWindowRow>,
    pub daily: Vec<DryRunDailyRow>,
    pub daily_by_window: Vec<DryRunDailyWindowRow>,
    #[serde(default)]
    pub hourly: Vec<DryRunHourlyRow>,
    #[serde(default)]
    pub hourly_by_window: Vec<DryRunHourlyWindowRow>,
    pub symbols: Vec<DryRunSymbolRow>,
    pub symbols_by_window: Vec<DryRunSymbolRow>,
    pub closed_trades: Vec<DryRunClosedTradeRow>,
    pub recent_closed: Vec<DryRunClosedTradeRow>,
    pub open_positions: Vec<DryRunOpenPositionRow>,
    #[serde(default)]
    pub execution_diagnostics: Option<DryRunExecutionDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunPerformanceReport {
    pub generated_at: String,
    pub summary: DryRunSummary,
    pub metrics: DryRunMetrics,
    pub equity_curve: Vec<DryRunEquityPoint>,
    pub by_window: Vec<DryRunWindowRow>,
    pub daily: Vec<DryRunDailyRow>,
    pub daily_by_window: Vec<DryRunDailyWindowRow>,
    #[serde(default)]
    pub hourly: Vec<DryRunHourlyRow>,
    #[serde(default)]
    pub hourly_by_window: Vec<DryRunHourlyWindowRow>,
    pub symbols: Vec<DryRunSymbolRow>,
    pub symbols_by_window: Vec<DryRunSymbolRow>,
    pub closed_trades: Vec<DryRunClosedTradeRow>,
    pub recent_closed: Vec<DryRunClosedTradeRow>,
    pub open_positions: Vec<DryRunOpenPositionRow>,
    pub strategies: Vec<DryRunStrategyReport>,
    pub pairing: DryRunPairingReport,
    #[serde(default)]
    pub execution_diagnostics: Option<DryRunExecutionDiagnostics>,
    #[serde(default)]
    pub runtime_evidence: Option<DryRunRuntimeEvidence>,
}

#[cfg(test)]
mod tests {
    use super::DryRunPerformanceReport;
    use serde_json::{json, Value};

    #[test]
    fn dry_run_report_roundtrip_preserves_diagnostics_fields() {
        let raw = json!({
            "generated_at": "2026-04-29T00:00:00Z",
            "summary": summary(),
            "metrics": metrics(),
            "equity_curve": [],
            "by_window": [],
            "daily": [],
            "daily_by_window": [],
            "hourly": [{
                "trading_hour_cst": "2026-04-29T08:00:00+08:00",
                "trade_count": 2,
                "closed_trade_count": 1,
                "wins": 1,
                "losses": 0,
                "net_pnl": 1.25,
                "cumulative_pnl": 1.25,
                "drawdown": 0.0
            }],
            "hourly_by_window": [{
                "trading_hour_cst": "2026-04-29T08:00:00+08:00",
                "window_secs": 300,
                "window_label": "5m",
                "trade_count": 2,
                "closed_trade_count": 1,
                "wins": 1,
                "losses": 0,
                "net_pnl": 1.25
            }],
            "symbols": [],
            "symbols_by_window": [],
            "closed_trades": [],
            "recent_closed": [],
            "open_positions": [],
            "strategies": [{
                "runtime_mode": "dryrun",
                "strategy_id": "pm5d",
                "deployment_id": "pm5d-dryrun",
                "label": "dryrun/pm5d/pm5d-dryrun",
                "summary": summary(),
                "metrics": metrics(),
                "equity_curve": [],
                "by_window": [],
                "daily": [],
                "daily_by_window": [],
                "hourly": [],
                "hourly_by_window": [],
                "symbols": [],
                "symbols_by_window": [],
                "closed_trades": [],
                "recent_closed": [],
                "open_positions": [],
                "execution_diagnostics": diagnostics()
            }],
            "pairing": {
                "pair_key": "runtime_mode,strategy_id,deployment_id,event_id",
                "mixed_event_groups": 0,
                "fills_in_mixed_event_groups": 0,
                "current_view_rows": 0,
                "side_aware_rows": 0
            },
            "execution_diagnostics": diagnostics(),
            "runtime_evidence": runtime_evidence()
        });

        let report: DryRunPerformanceReport = serde_json::from_value(raw).unwrap();
        let roundtripped = serde_json::to_value(report).unwrap();

        assert_eq!(
            roundtripped["metrics"]["sharpe_basis"],
            "closed_trade_pnl_sqrt_n"
        );
        assert_eq!(
            roundtripped["metrics"]["daily_sharpe_basis"],
            "daily_net_pnl_sqrt_365"
        );
        assert_eq!(
            roundtripped["execution_diagnostics"]["basis"],
            "strategy_runtime_orders"
        );
        assert_eq!(
            roundtripped["strategies"][0]["execution_diagnostics"]["basis"],
            "strategy_runtime_orders"
        );
        assert_eq!(
            roundtripped["execution_diagnostics"]["summary"]["rejected_buy_orders"],
            2
        );
        assert_eq!(
            roundtripped["runtime_evidence"]["basis"],
            "strategy_runtime_orders_fills_and_events"
        );
        assert_eq!(
            roundtripped["runtime_evidence"]["events"][0]["event_id"],
            "event-1"
        );
        assert_eq!(
            roundtripped["runtime_evidence"]["orders"][0]["intent_id"],
            "intent-1"
        );
        assert_eq!(roundtripped["hourly"][0]["trade_count"], 2);
        assert_eq!(roundtripped["hourly_by_window"][0]["window_label"], "5m");
    }

    fn summary() -> Value {
        json!({
            "total_trades": 1,
            "closed_trades": 1,
            "wins": 1,
            "losses": 0,
            "win_rate_pct": 100.0,
            "realized_pnl": 1.25,
            "total_fees": 0.01,
            "open_positions": 0,
            "open_exposure": 0.0,
            "latest_opened_at": null,
            "latest_closed_at": null
        })
    }

    fn metrics() -> Value {
        json!({
            "sharpe": 1.5,
            "sharpe_per_trade": 1.5,
            "sharpe_basis": "closed_trade_pnl_sqrt_n",
            "closed_trade_count_for_sharpe": 3,
            "sharpe_daily_ann": 2.5,
            "daily_sharpe_basis": "daily_net_pnl_sqrt_365",
            "profit_factor": "Infinity",
            "max_drawdown": -0.25,
            "avg_trade": 0.42,
            "gross_profit": 1.25,
            "gross_loss": 0.0,
            "equity_points": 1
        })
    }

    fn diagnostics() -> Value {
        json!({
            "basis": "strategy_runtime_orders",
            "partial_buy_threshold_pct": 98,
            "summary": {
                "total_orders": 7,
                "buy_orders": 5,
                "rejected_buy_orders": 2,
                "buy_fill_rate_pct": 61.5
            },
            "strategies": [{
                "runtime_mode": "dryrun",
                "strategy_id": "pm5d",
                "deployment_id": "pm5d-dryrun",
                "buy_orders": 5,
                "rejected_buy_orders": 2
            }]
        })
    }

    fn runtime_evidence() -> Value {
        json!({
            "schema_version": 1,
            "basis": "strategy_runtime_orders_fills_and_events",
            "events": [{
                "deployment_id": "pm5d-dryrun",
                "event_id": "event-1",
                "decision_ts": "2026-05-02T01:02:03Z",
                "quote": "0.42",
                "signal_inputs": {"purpose": "ENTRY"},
                "side": "BUY",
                "entry_price": "0.42",
                "fill_status": "FILLED",
                "settlement": "open",
                "pnl": "0"
            }],
            "orders": [{
                "deployment_id": "pm5d-dryrun",
                "intent_id": "intent-1",
                "order_id": "order-1",
                "token_id": "token-up",
                "quantity": "10",
                "limit_price": "0.42",
                "filled_quantity": "10",
                "status": "FILLED"
            }],
            "fills": [{
                "deployment_id": "pm5d-dryrun",
                "intent_id": "intent-1",
                "order_id": "order-1",
                "fill_id": "fill-1",
                "token_id": "token-up",
                "fill_side": "BUY",
                "quantity": "10",
                "price": "0.42",
                "fee": "0",
                "fill_timestamp": "2026-05-02T01:02:03Z"
            }]
        })
    }
}
