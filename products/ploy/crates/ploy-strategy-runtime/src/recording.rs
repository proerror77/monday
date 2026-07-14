use async_trait::async_trait;
use ploy_strategy_bundles::{ExecutionReport, NullRecorder, Recorder, RuntimeMode, SignalRecord};
use ploy_trading::{FillRecord, TradeSide, TradingIntent};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap;
use tracing::info;

#[derive(Clone, Default)]
struct TokenExecutionContext {
    event_id: Option<String>,
    symbol: Option<String>,
    market_side: Option<String>,
}

struct RuntimeDbRecorder {
    pool: sqlx::PgPool,
    mode_label: String,
    token_context: HashMap<String, TokenExecutionContext>,
}

impl RuntimeDbRecorder {
    fn new(pool: sqlx::PgPool, mode_label: String) -> Self {
        Self {
            pool,
            mode_label,
            token_context: HashMap::new(),
        }
    }

    fn merge_context(
        &self,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
    ) -> TokenExecutionContext {
        let mut context = self
            .token_context
            .get(&intent.token_id)
            .cloned()
            .unwrap_or_default();

        if context.event_id.is_none() && !intent.market_id.is_empty() {
            context.event_id = Some(intent.market_id.clone());
        }

        if let Some(signal) = signal {
            if context.event_id.is_none() {
                context.event_id = signal.event_id.clone();
            }
            if context.symbol.is_none() {
                context.symbol = Some(signal.symbol.clone());
            }
            if context.market_side.is_none() {
                context.market_side = Some(signal.direction.clone());
            }
        }

        context
    }

    fn remember_context(&mut self, token_id: &str, context: &TokenExecutionContext) {
        if context.event_id.is_none() && context.symbol.is_none() && context.market_side.is_none() {
            return;
        }
        self.token_context
            .insert(token_id.to_string(), context.clone());
    }

    fn remember_signal_context(&mut self, signal: &SignalRecord) {
        let Some(token_id) = signal.token_id.as_deref() else {
            return;
        };

        self.token_context.insert(
            token_id.to_string(),
            TokenExecutionContext {
                event_id: signal.event_id.clone(),
                symbol: Some(signal.symbol.clone()),
                market_side: Some(signal.direction.clone()),
            },
        );
    }

    async fn persist_signal(&self, signal: &SignalRecord) -> Result<(), String> {
        let confidence = Decimal::from_f64(signal.p_hat);
        let edge = Decimal::from_f64(signal.edge);
        let context = json!({
            "runtime_mode": self.mode_label,
            "event_id": signal.event_id,
            "intent_id": signal.intent_id,
        });

        sqlx::query(
            r#"
            INSERT INTO signal_history (
                recorded_at,
                intent_id,
                agent_id,
                strategy_id,
                domain,
                signal_type,
                token_id,
                symbol,
                side,
                confidence,
                market_price,
                edge,
                context
            )
            VALUES (
                $1,
                NULL,
                'ploy-runner',
                $2,
                'polymarket',
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10
            )
            "#,
        )
        .bind(signal.ts)
        .bind(signal.strategy.as_str())
        .bind(signal.decision.as_str())
        .bind(signal.token_id.as_deref())
        .bind(signal.symbol.as_str())
        .bind(signal.direction.as_str())
        .bind(confidence)
        .bind(signal.entry_price)
        .bind(edge)
        .bind(context)
        .execute(&self.pool)
        .await
        .map_err(|error| format!("persist signal record: {error}"))?;
        Ok(())
    }

    async fn persist_order(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) -> Result<(), String> {
        let fill = report.fill.as_ref();
        let status = if report.rejected {
            "REJECTED"
        } else if let Some(fill) = fill {
            if fill.quantity >= intent.quantity {
                "FILLED"
            } else {
                "PARTIALLY_FILLED"
            }
        } else {
            "ACKNOWLEDGED"
        };
        let exchange_order_id = if report.order_id.is_empty() || report.order_id == order_id {
            None
        } else {
            Some(report.order_id.as_str())
        };
        let filled_quantity = fill.map(|record| record.quantity).unwrap_or(Decimal::ZERO);
        let avg_fill_price = fill.map(|record| record.price);
        let runtime_price_basis = report.price_basis.unwrap_or("unknown");
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "signal_decision": signal.map(|record| record.decision.as_str()),
            "signal_strategy": signal.map(|record| record.strategy.as_str()),
            "signal_symbol": signal.map(|record| record.symbol.as_str()),
            "signal_direction": signal.map(|record| record.direction.as_str()),
            "signal_p_hat": signal.map(|record| record.p_hat),
            "signal_edge": signal.map(|record| record.edge),
            "signal_entry_price": signal.map(|record| record.entry_price.to_string()),
            "runtime_price_basis": runtime_price_basis,
            "full_depth_runtime_parity": runtime_price_basis == "full_depth_sweep",
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        sqlx::query(
            r#"
            INSERT INTO strategy_runtime_orders (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                venue_order_id,
                event_id,
                symbol,
                token_id,
                market_side,
                order_side,
                quantity,
                limit_price,
                filled_quantity,
                avg_fill_price,
                status,
                rejection_reason,
                slippage,
                market_impact,
                context
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
            )
            ON CONFLICT (order_id) DO UPDATE
            SET venue_order_id = COALESCE(EXCLUDED.venue_order_id, strategy_runtime_orders.venue_order_id),
                event_id = COALESCE(EXCLUDED.event_id, strategy_runtime_orders.event_id),
                symbol = COALESCE(EXCLUDED.symbol, strategy_runtime_orders.symbol),
                market_side = COALESCE(EXCLUDED.market_side, strategy_runtime_orders.market_side),
                filled_quantity = CASE
                    WHEN EXCLUDED.filled_quantity > 0 THEN
                        strategy_runtime_orders.filled_quantity + EXCLUDED.filled_quantity
                    ELSE strategy_runtime_orders.filled_quantity
                END,
                avg_fill_price = CASE
                    WHEN EXCLUDED.filled_quantity > 0 AND EXCLUDED.avg_fill_price IS NOT NULL THEN
                        (
                            COALESCE(
                                strategy_runtime_orders.avg_fill_price,
                                EXCLUDED.avg_fill_price
                            ) * strategy_runtime_orders.filled_quantity
                            + EXCLUDED.avg_fill_price * EXCLUDED.filled_quantity
                        ) / NULLIF(
                            strategy_runtime_orders.filled_quantity + EXCLUDED.filled_quantity,
                            0
                        )
                    ELSE COALESCE(EXCLUDED.avg_fill_price, strategy_runtime_orders.avg_fill_price)
                END,
                status = EXCLUDED.status,
                rejection_reason = COALESCE(EXCLUDED.rejection_reason, strategy_runtime_orders.rejection_reason),
                slippage = COALESCE(EXCLUDED.slippage, strategy_runtime_orders.slippage),
                market_impact = COALESCE(EXCLUDED.market_impact, strategy_runtime_orders.market_impact),
                context = EXCLUDED.context
            "#,
        )
        .bind(intent.created_at)
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(order_id)
        .bind(exchange_order_id)
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(intent.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match intent.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(intent.quantity)
        .bind(intent.limit_price)
        .bind(filled_quantity)
        .bind(avg_fill_price)
        .bind(status)
        .bind(report.rejection_reason.as_deref())
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(context_json)
        .execute(&self.pool)
        .await
        .map_err(|error| format!("persist execution order {order_id}: {error}"))?;
        Ok(())
    }

    async fn persist_fill(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) -> Result<(), String> {
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        sqlx::query(
            r#"
            INSERT INTO strategy_runtime_fills (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                fill_id,
                event_id,
                symbol,
                token_id,
                market_side,
                fill_side,
                quantity,
                price,
                fee,
                slippage,
                market_impact,
                fill_timestamp,
                context
            )
            VALUES (
                NOW(), $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (fill_id) DO NOTHING
            "#,
        )
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(fill.order_id.as_str())
        .bind(fill.fill_id.as_str())
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(fill.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match fill.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(fill.quantity)
        .bind(fill.price)
        .bind(fill.fee)
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(fill.timestamp)
        .bind(context_json)
        .execute(&self.pool)
        .await
        .map_err(|error| format!("persist execution fill {}: {error}", fill.fill_id))?;
        Ok(())
    }
}

#[async_trait]
impl Recorder for RuntimeDbRecorder {
    async fn record_signal(&mut self, signal: &SignalRecord) -> Result<(), String> {
        self.remember_signal_context(signal);
        self.persist_signal(signal).await
    }

    async fn record_order(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) -> Result<(), String> {
        if self.mode_label == "live" {
            return Ok(());
        }
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_order(strategy, intent, &context, signal, report, order_id)
            .await
    }

    async fn record_fill(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) -> Result<(), String> {
        if self.mode_label == "live" {
            return Ok(());
        }
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_fill(strategy, intent, &context, fill, report)
            .await
    }

    async fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) fn build_signal_recorder(
    db_pool: Option<sqlx::PgPool>,
    mode: RuntimeMode,
) -> Box<dyn Recorder> {
    let Some(pool) = db_pool else {
        info!("Signal recorder disabled — DATABASE_URL unavailable");
        return Box::new(NullRecorder);
    };

    let mode_label = match mode {
        RuntimeMode::Backtest => "backtest",
        RuntimeMode::Replay => "replay",
        RuntimeMode::DryRun => "dry_run",
        RuntimeMode::Live => "live",
    }
    .to_string();

    Box::new(RuntimeDbRecorder::new(pool, mode_label))
}
