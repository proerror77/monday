//! Portfolio Core（會計真相源）
//! - 根據 ExecutionEvent（fills/fees/funding）更新帳戶狀態
//! - 發佈只讀 AccountView 快照（Arc 快照）
//! - 多帳戶 PnL 聚合（跨交易所）

pub mod multi_account;
pub mod prediction;

pub use multi_account::{
    AccountId, AccountPnl, AggregatedAccountView, AggregatedPosition, MultiAccountPortfolio,
    MultiAccountState, PnlReport,
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use hft_core::{OrderId, Price, Quantity, Side, Symbol};
use ports::{AccountView, ExecutionEvent, Position};
use snapshot::SnapshotContainer;
use tracing::{info, warn};

const ACCOUNTING_EVENT_REPLAY_CAPACITY: usize = 200_000;

/// An observed accounting event that was intentionally not applied because
/// the portfolio cannot prove the event belongs to its canonical state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationException {
    pub order_id: Option<OrderId>,
    pub reason: String,
    pub event: Option<ExecutionEvent>,
}

/// Portfolio state that can be persisted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioState {
    pub account_view: AccountView,
    #[serde(default)]
    pub total_fees: Decimal,
    pub order_meta: HashMap<OrderId, (Symbol, Side)>,
    pub market_prices: HashMap<Symbol, Price>,
    /// 已處理的成交ID（去重），恢復後避免重覆累計
    #[serde(default)]
    pub processed_fill_ids: HashMap<OrderId, HashSet<String>>,
    /// Fee identities are a separate namespace from venue fill identities.
    #[serde(default)]
    pub processed_fee_ids: HashMap<OrderId, HashSet<String>>,
    /// Accounting event keys ordered oldest to newest for deterministic bounded recovery.
    #[serde(default)]
    pub recent_accounting_event_ids: Vec<(OrderId, String)>,
    #[serde(default)]
    pub reconciliation_exceptions: Vec<ReconciliationException>,
    #[serde(default)]
    pub canonical_state_digest: Option<String>,
}

impl PortfolioState {
    pub fn refresh_canonical_digest(&mut self) {
        self.canonical_state_digest = Some(canonical_state_digest(self));
    }
}

/// 最小 Portfolio：單帳戶，根據 fills 更新倉位/現金與 PnL
pub struct Portfolio {
    view: AccountView,
    snapshot: SnapshotContainer<AccountView>,
    // 供查詢：order_id → (symbol, side)
    order_meta: HashMap<hft_core::OrderId, (Symbol, Side)>,
    // 緩存最新市場價格用於 mark-to-market
    market_prices: HashMap<Symbol, Price>,
    // 已處理的成交 ID（去重）
    processed_fill_ids: HashMap<hft_core::OrderId, HashSet<String>>,
    processed_fee_ids: HashMap<hft_core::OrderId, HashSet<String>>,
    total_fees: Decimal,
    // Bounded chronological journal used to restore the engine replay horizon.
    recent_accounting_event_ids: VecDeque<(OrderId, String)>,
    reconciliation_exceptions: Vec<ReconciliationException>,
}

impl Default for Portfolio {
    fn default() -> Self {
        let view = AccountView::default();
        let snapshot = SnapshotContainer::new(view.clone());
        Self {
            view,
            snapshot,
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
            processed_fill_ids: HashMap::new(),
            processed_fee_ids: HashMap::new(),
            total_fees: Decimal::ZERO,
            recent_accounting_event_ids: VecDeque::new(),
            reconciliation_exceptions: Vec::new(),
        }
    }
}

impl Portfolio {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cash_balance(cash_balance: Decimal) -> Self {
        let mut portfolio = Self::default();
        portfolio.view.cash_balance = cash_balance;
        if cash_balance > Decimal::ZERO {
            portfolio.view.high_water_mark = cash_balance;
        }
        portfolio.snapshot.store(Arc::new(portfolio.view.clone()));
        portfolio
    }

    /// 註冊下單元資訊（供 fill 時查找 symbol/side）
    pub fn register_order(&mut self, order_id: hft_core::OrderId, symbol: Symbol, side: Side) {
        if let Some((existing_symbol, existing_side)) = self.order_meta.get(&order_id) {
            if existing_symbol == &symbol && *existing_side == side {
                return;
            }
            self.record_exception(
                Some(order_id),
                "conflicting portfolio order metadata registration",
                None,
            );
            return;
        }
        self.order_meta.insert(order_id, (symbol, side));
    }

    fn record_exception(
        &mut self,
        order_id: Option<OrderId>,
        reason: impl Into<String>,
        event: Option<&ExecutionEvent>,
    ) {
        self.reconciliation_exceptions
            .push(ReconciliationException {
                order_id,
                reason: reason.into(),
                event: event.cloned(),
            });
    }

    pub fn reconciliation_exceptions(&self) -> &[ReconciliationException] {
        &self.reconciliation_exceptions
    }

    /// 處理執行事件，僅處理 Fill/Fee/Balance 類事件
    pub fn on_execution_event(&mut self, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::Fill {
                order_id,
                price,
                quantity,
                fill_id,
                ..
            } => {
                if quantity.0 <= Decimal::ZERO {
                    warn!(
                        order_id = %order_id.0,
                        quantity = %quantity.0,
                        "ignoring fill with non-positive quantity"
                    );
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill has non-positive quantity",
                        Some(event),
                    );
                    return;
                }
                if price.0 <= Decimal::ZERO {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill has non-positive price",
                        Some(event),
                    );
                    return;
                }
                if fill_id.is_empty() {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill is missing the venue execution identity",
                        Some(event),
                    );
                    return;
                }
                let Some((symbol, side)) = self.order_meta.get(order_id).cloned() else {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill references an unknown portfolio order",
                        Some(event),
                    );
                    return;
                };
                // De-duplication is scoped to the fill namespace only.
                let is_new = self
                    .processed_fill_ids
                    .entry(order_id.clone())
                    .or_default()
                    .insert(fill_id.clone());
                if is_new {
                    self.record_accounting_event(order_id.clone(), format!("fill:{fill_id}"));
                    self.apply_fill(&symbol, side, *price, *quantity);
                    // Fill price is the freshest executable fallback mark observed by this ledger.
                    self.market_prices.insert(symbol.clone(), *price);
                    self.recalculate_unrealized_pnl();
                }
            }
            ExecutionEvent::FeeCharged {
                order_id,
                amount,
                fill_id,
                ..
            } => {
                if *amount < Decimal::ZERO {
                    warn!(order_id = %order_id.0, amount = %amount, "ignoring negative fee");
                    self.record_exception(
                        Some(order_id.clone()),
                        "fee amount is negative",
                        Some(event),
                    );
                    return;
                }
                if self.order_meta.contains_key(order_id) {
                    if fill_id.is_empty() {
                        self.record_exception(
                            Some(order_id.clone()),
                            "fee is missing the venue execution identity",
                            Some(event),
                        );
                        return;
                    }
                    let fee_id = format!("fee:{fill_id}");
                    let inserted = self
                        .processed_fee_ids
                        .entry(order_id.clone())
                        .or_default()
                        .insert(fee_id.clone());
                    if inserted {
                        if !fill_id.is_empty() {
                            self.record_accounting_event(order_id.clone(), fee_id);
                        }
                        self.view.cash_balance -= *amount;
                        self.view.realized_pnl -= *amount;
                        self.total_fees += *amount;
                        self.update_drawdown_stats();
                    }
                } else {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fee references an unknown portfolio order",
                        Some(event),
                    );
                }
            }
            _ => {}
        }
        // 每次更新後發佈只讀快照
        self.snapshot.store(Arc::new(self.view.clone()));
    }

    pub fn reader(&self) -> Arc<dyn snapshot::SnapshotReader<AccountView>> {
        self.snapshot.reader()
    }

    fn record_accounting_event(&mut self, order_id: OrderId, event_id: String) {
        if self.recent_accounting_event_ids.len() >= ACCOUNTING_EVENT_REPLAY_CAPACITY {
            self.recent_accounting_event_ids.pop_front();
        }
        self.recent_accounting_event_ids
            .push_back((order_id, event_id));
    }

    /// 更新市場價格並重新計算未實現盈虧
    pub fn update_market_prices(&mut self, prices: &HashMap<Symbol, Price>) {
        // 更新價格緩存
        for (symbol, price) in prices {
            self.market_prices.insert(symbol.clone(), *price);
        }

        // 重新計算所有持倉的未實現盈虧
        self.recalculate_unrealized_pnl();

        // 發佈更新後的快照
        self.snapshot.store(Arc::new(self.view.clone()));
    }

    /// 根據市場中間價重新計算未實現盈虧
    fn recalculate_unrealized_pnl(&mut self) {
        let mut total_unrealized = Decimal::ZERO;

        for (symbol, position) in &mut self.view.positions {
            if let Some(market_price) = self.market_prices.get(symbol) {
                // 未實現盈虧 = (市場價 - 均價) * 持倉量
                // 注意：賣空倉位的 quantity 為負數
                let unrealized = (market_price.0 - position.avg_price.0) * position.quantity.0;
                position.unrealized_pnl = unrealized;
                total_unrealized += unrealized;
            }
        }

        self.view.unrealized_pnl = total_unrealized;

        // 更新回撤統計
        self.update_drawdown_stats();
    }

    /// 更新回撤統計 (高水位、當前回撤、最大回撤)
    fn update_drawdown_stats(&mut self) {
        let equity = self.view.equity();

        // 更新高水位標記
        if equity > self.view.high_water_mark {
            self.view.high_water_mark = equity;
        }

        // 計算當前回撤百分比
        if self.view.high_water_mark > Decimal::ZERO {
            let drawdown = self.view.high_water_mark - equity;
            // 避免除以零，計算回撤百分比
            let dd_pct = (drawdown / self.view.high_water_mark * Decimal::from(100))
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            self.view.drawdown_pct = dd_pct.max(0.0); // 確保非負

            // 更新歷史最大回撤
            if self.view.drawdown_pct > self.view.max_drawdown_pct {
                self.view.max_drawdown_pct = self.view.drawdown_pct;
            }
        } else {
            self.view.drawdown_pct = 0.0;
        }
    }

    fn apply_fill(&mut self, symbol: &Symbol, side: Side, price: Price, qty: Quantity) {
        let pos = self
            .view
            .positions
            .entry(symbol.clone())
            .or_insert(Position {
                symbol: symbol.clone(),
                quantity: Quantity::zero(),
                avg_price: Price::zero(),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            });

        let old_qty = pos.quantity.0;
        let delta = match side {
            Side::Buy => qty.0,
            Side::Sell => -qty.0,
        };
        let new_qty = old_qty + delta;
        let increases_same_side = old_qty == Decimal::ZERO
            || (old_qty > Decimal::ZERO && delta > Decimal::ZERO)
            || (old_qty < Decimal::ZERO && delta < Decimal::ZERO);

        if increases_same_side {
            let gross_quantity = old_qty.abs() + delta.abs();
            pos.avg_price =
                Price((pos.avg_price.0 * old_qty.abs() + price.0 * delta.abs()) / gross_quantity);
        } else {
            let closed_quantity = old_qty.abs().min(delta.abs());
            let direction = if old_qty > Decimal::ZERO {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            let realized_delta = (price.0 - pos.avg_price.0) * closed_quantity * direction;
            pos.realized_pnl += realized_delta;
            self.view.realized_pnl += realized_delta;

            if new_qty == Decimal::ZERO {
                pos.avg_price = Price::zero();
            } else if (new_qty > Decimal::ZERO) != (old_qty > Decimal::ZERO) {
                pos.avg_price = price;
            }
        }

        pos.quantity = Quantity(new_qty);
        self.view.cash_balance -= price.0 * delta;
        if new_qty == Decimal::ZERO {
            self.view.positions.remove(symbol);
        }
    }

    /// Export portfolio state for persistence
    pub fn export_state(&self) -> PortfolioState {
        let mut state = PortfolioState {
            account_view: self.view.clone(),
            total_fees: self.total_fees,
            order_meta: self.order_meta.clone(),
            market_prices: self.market_prices.clone(),
            processed_fill_ids: self.processed_fill_ids.clone(),
            processed_fee_ids: self.processed_fee_ids.clone(),
            recent_accounting_event_ids: self.recent_accounting_event_ids.iter().cloned().collect(),
            reconciliation_exceptions: self.reconciliation_exceptions.clone(),
            canonical_state_digest: None,
        };
        state.canonical_state_digest = Some(canonical_state_digest(&state));
        state
    }

    /// Import portfolio state from persistent storage
    pub fn import_state(&mut self, state: PortfolioState) {
        info!(
            "Importing portfolio state - Cash: {}, Positions: {}, Orders: {}",
            state.account_view.cash_balance,
            state.account_view.positions.len(),
            state.order_meta.len()
        );

        if let Err(reason) = self.try_import_state(state) {
            self.record_exception(None, reason, None);
            warn!("refused invalid portfolio state during restore");
        }
    }

    pub fn try_import_state(&mut self, state: PortfolioState) -> Result<(), String> {
        validate_state(&state)?;

        self.view = state.account_view;
        self.total_fees = state.total_fees;
        self.order_meta = state.order_meta;
        self.market_prices = state.market_prices;
        self.processed_fill_ids = state.processed_fill_ids;
        self.processed_fee_ids = state.processed_fee_ids;
        self.reconciliation_exceptions = state.reconciliation_exceptions;
        // Older snapshots only carried the bounded journal.  Reconstructing
        // the fee namespace from that journal keeps replay idempotent without
        // treating a fill id and fee id as the same key.
        for (order_id, event_id) in &state.recent_accounting_event_ids {
            if let Some(fee_id) = event_id.strip_prefix("fee:") {
                self.processed_fee_ids
                    .entry(order_id.clone())
                    .or_default()
                    .insert(event_id.clone());
                if fee_id.is_empty() {
                    self.reconciliation_exceptions
                        .push(ReconciliationException {
                            order_id: Some(order_id.clone()),
                            reason: "snapshot contains an empty fee identity".to_string(),
                            event: None,
                        });
                }
            }
        }
        self.recent_accounting_event_ids = state
            .recent_accounting_event_ids
            .into_iter()
            .rev()
            .take(ACCOUNTING_EVENT_REPLAY_CAPACITY)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        // Recalculate unrealized PnL with current market prices
        self.recalculate_unrealized_pnl();

        // Update snapshot
        self.snapshot.store(Arc::new(self.view.clone()));

        // Log summary
        info!(
            "Portfolio state imported - Total value: {}, Realized PnL: {}, Unrealized PnL: {}",
            self.view.equity(),
            self.view.realized_pnl,
            self.view.unrealized_pnl
        );
        Ok(())
    }
}

fn validate_state(state: &PortfolioState) -> Result<(), String> {
    if let Some(expected) = &state.canonical_state_digest {
        let actual = canonical_state_digest(state);
        if expected != &actual {
            return Err("portfolio canonical financial checkpoint digest mismatch".to_string());
        }
    }
    for (symbol, position) in &state.account_view.positions {
        if symbol != &position.symbol {
            return Err("portfolio position key does not match position symbol".to_string());
        }
    }
    for order_id in state.processed_fill_ids.keys() {
        if !state.order_meta.contains_key(order_id) {
            return Err(format!(
                "processed fill namespace references unknown order {}",
                order_id.0
            ));
        }
    }
    for order_id in state.processed_fee_ids.keys() {
        if !state.order_meta.contains_key(order_id) {
            return Err(format!(
                "processed fee namespace references unknown order {}",
                order_id.0
            ));
        }
    }
    for (order_id, event_id) in &state.recent_accounting_event_ids {
        if !state.order_meta.contains_key(order_id) {
            return Err(format!(
                "accounting journal references unknown order {}",
                order_id.0
            ));
        }
        if event_id.is_empty() || (!event_id.starts_with("fill:") && !event_id.starts_with("fee:"))
        {
            return Err("accounting journal contains an untyped event identity".to_string());
        }
    }
    Ok(())
}

fn canonical_state_digest(state: &PortfolioState) -> String {
    let mut material = String::new();
    let view = &state.account_view;
    material.push_str(&format!(
        "cash={};realized={};unrealized={};fees={};hwm={};dd={};maxdd={};session={};",
        view.cash_balance,
        view.realized_pnl,
        view.unrealized_pnl,
        state.total_fees,
        view.high_water_mark,
        view.drawdown_pct,
        view.max_drawdown_pct,
        view.session_start_us
    ));
    let mut positions = view.positions.iter().collect::<Vec<_>>();
    positions.sort_by_key(|(symbol, _)| symbol.as_str().to_string());
    for (symbol, position) in positions {
        material.push_str(&format!(
            "pos:{}:{}:{}:{}:{};",
            symbol.as_str(),
            position.quantity.0,
            position.avg_price.0,
            position.unrealized_pnl,
            position.realized_pnl
        ));
    }
    let mut orders = state.order_meta.iter().collect::<Vec<_>>();
    orders.sort_by_key(|(order_id, _)| order_id.0.clone());
    for (order_id, (symbol, side)) in orders {
        material.push_str(&format!(
            "order:{}:{}:{:?};",
            order_id.0,
            symbol.as_str(),
            side
        ));
    }
    let mut marks = state.market_prices.iter().collect::<Vec<_>>();
    marks.sort_by_key(|(symbol, _)| symbol.as_str().to_string());
    for (symbol, price) in marks {
        material.push_str(&format!("mark:{}:{};", symbol.as_str(), price.0));
    }
    append_namespaced_ids(&mut material, "fill", &state.processed_fill_ids);
    append_namespaced_ids(&mut material, "fee", &state.processed_fee_ids);
    for (order_id, event_id) in &state.recent_accounting_event_ids {
        material.push_str(&format!("journal:{}:{};", order_id.0, event_id));
    }
    for exception in &state.reconciliation_exceptions {
        material.push_str(&format!(
            "exception:{:?}:{:?}:{};",
            exception.order_id, exception.event, exception.reason
        ));
    }
    format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
}

fn append_namespaced_ids(
    material: &mut String,
    namespace: &str,
    ids: &HashMap<OrderId, HashSet<String>>,
) {
    let mut entries = ids
        .iter()
        .flat_map(|(order_id, values)| {
            values
                .iter()
                .map(move |value| (order_id.0.clone(), value.clone()))
        })
        .collect::<Vec<_>>();
    entries.sort();
    for (order_id, value) in entries {
        material.push_str(&format!("{}:{}:{};", namespace, order_id, value));
    }
}

/// 實現 PortfolioManager trait - 將現有方法適配為 trait 接口
impl ports::PortfolioManager for Portfolio {
    fn register_order(&mut self, order_id: hft_core::OrderId, symbol: Symbol, side: Side) -> bool {
        // 直接調用現有實現
        let before = self.reconciliation_exceptions.len();
        self.register_order(order_id, symbol, side);
        self.reconciliation_exceptions.len() == before
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 直接調用現有實現
        self.on_execution_event(event);
    }

    fn reader(&self) -> Arc<dyn snapshot::SnapshotReader<AccountView>> {
        // 直接調用現有實現
        self.reader()
    }

    fn update_market_prices(&mut self, prices: &HashMap<Symbol, Price>) {
        // 直接調用現有實現
        self.update_market_prices(prices);
    }

    fn export_state(&self) -> ports::PortfolioState {
        // 轉換內部 PortfolioState 為 ports::PortfolioState
        let internal_state = self.export_state();
        ports::PortfolioState {
            account_view: internal_state.account_view,
            total_fees: internal_state.total_fees,
            order_meta: internal_state.order_meta,
            market_prices: internal_state.market_prices,
            processed_fill_ids: internal_state.processed_fill_ids,
            processed_fee_ids: internal_state.processed_fee_ids,
            recent_accounting_event_ids: internal_state.recent_accounting_event_ids,
            reconciliation_exceptions: internal_state
                .reconciliation_exceptions
                .into_iter()
                .map(|exception| ports::ReconciliationException {
                    order_id: exception.order_id,
                    reason: exception.reason,
                    event: exception.event,
                })
                .collect(),
            canonical_state_digest: internal_state.canonical_state_digest,
        }
    }

    fn import_state(&mut self, state: ports::PortfolioState) {
        // 轉換 ports::PortfolioState 為內部 PortfolioState
        let internal_state = PortfolioState {
            account_view: state.account_view,
            total_fees: state.total_fees,
            order_meta: state.order_meta,
            market_prices: state.market_prices,
            processed_fill_ids: state.processed_fill_ids,
            processed_fee_ids: state.processed_fee_ids,
            recent_accounting_event_ids: state.recent_accounting_event_ids,
            reconciliation_exceptions: state
                .reconciliation_exceptions
                .into_iter()
                .map(|exception| ReconciliationException {
                    order_id: exception.order_id,
                    reason: exception.reason,
                    event: exception.event,
                })
                .collect(),
            canonical_state_digest: state.canonical_state_digest,
        };
        self.import_state(internal_state);
    }

    fn try_import_state(&mut self, state: ports::PortfolioState) -> Result<(), String> {
        let internal_state = PortfolioState {
            account_view: state.account_view,
            total_fees: state.total_fees,
            order_meta: state.order_meta,
            market_prices: state.market_prices,
            processed_fill_ids: state.processed_fill_ids,
            processed_fee_ids: state.processed_fee_ids,
            recent_accounting_event_ids: state.recent_accounting_event_ids,
            reconciliation_exceptions: state
                .reconciliation_exceptions
                .into_iter()
                .map(|exception| ReconciliationException {
                    order_id: exception.order_id,
                    reason: exception.reason,
                    event: exception.event,
                })
                .collect(),
            canonical_state_digest: state.canonical_state_digest,
        };
        Portfolio::try_import_state(self, internal_state)
    }

    fn reconciliation_exception_count(&self) -> usize {
        self.reconciliation_exceptions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, Price, Quantity};

    fn fill(
        portfolio: &mut Portfolio,
        order: &str,
        fill_id: &str,
        symbol: &Symbol,
        side: Side,
        price: i64,
        quantity: i64,
    ) {
        let order_id = OrderId(order.into());
        portfolio.register_order(order_id.clone(), symbol.clone(), side);
        portfolio.on_execution_event(&ExecutionEvent::Fill {
            order_id,
            price: Price(Decimal::from(price)),
            quantity: Quantity(Decimal::from(quantity)),
            timestamp: 0,
            fill_id: fill_id.into(),
        });
    }

    fn mark(portfolio: &mut Portfolio, symbol: &Symbol, price: i64) {
        portfolio.update_market_prices(&HashMap::from([(
            symbol.clone(),
            Price(Decimal::from(price)),
        )]));
    }

    #[test]
    fn venue_fee_is_idempotent_and_reduces_cash_and_realized_pnl() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(100));
        let order_id = OrderId("fee-order".into());
        portfolio.register_order(order_id.clone(), Symbol::new("123"), Side::Buy);
        let fee = ExecutionEvent::FeeCharged {
            order_id,
            amount: Decimal::new(175, 2),
            timestamp: 0,
            fill_id: "fill-1".into(),
        };

        portfolio.on_execution_event(&fee);
        portfolio.on_execution_event(&fee);

        let view = portfolio.reader().load();
        assert_eq!(view.cash_balance, Decimal::new(9825, 2));
        assert_eq!(view.realized_pnl, Decimal::new(-175, 2));
    }

    #[test]
    fn long_open_increase_reduce_and_close_preserve_accounting_identity() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let symbol = Symbol::new("BTCUSDT");

        fill(&mut portfolio, "B-1", "f1", &symbol, Side::Buy, 100, 2);
        fill(&mut portfolio, "B-2", "f2", &symbol, Side::Buy, 130, 1);

        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::from(3));
        assert_eq!(position.avg_price.0, Decimal::from(110));
        assert_eq!(view.cash_balance, Decimal::from(670));
        assert_eq!(view.realized_pnl, Decimal::ZERO);
        assert_eq!(view.unrealized_pnl, Decimal::from(60));
        assert_eq!(view.equity(), Decimal::from(1060));

        fill(&mut portfolio, "S-1", "f3", &symbol, Side::Sell, 140, 1);
        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::from(2));
        assert_eq!(position.avg_price.0, Decimal::from(110));
        assert_eq!(position.realized_pnl, Decimal::from(30));
        assert_eq!(view.realized_pnl, Decimal::from(30));

        fill(&mut portfolio, "S-2", "f4", &symbol, Side::Sell, 90, 2);
        mark(&mut portfolio, &symbol, 90);
        let view = portfolio.reader().load();
        assert!(!view.positions.contains_key(&symbol));
        assert!(view.positions.is_empty());
        assert_eq!(view.realized_pnl, Decimal::from(-10));
        assert_eq!(view.cash_balance, Decimal::from(990));
        assert_eq!(view.equity(), Decimal::from(990));
    }

    #[test]
    fn short_open_increase_reduce_and_cross_to_long_are_signed_correctly() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let symbol = Symbol::new("ETHUSDT");

        fill(&mut portfolio, "S-1", "f1", &symbol, Side::Sell, 100, 1);
        fill(&mut portfolio, "S-2", "f2", &symbol, Side::Sell, 120, 1);
        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::from(-2));
        assert_eq!(position.avg_price.0, Decimal::from(110));
        assert_eq!(view.realized_pnl, Decimal::ZERO);
        assert_eq!(view.cash_balance, Decimal::from(1220));

        fill(&mut portfolio, "B-1", "f3", &symbol, Side::Buy, 90, 1);
        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::from(-1));
        assert_eq!(position.avg_price.0, Decimal::from(110));
        assert_eq!(position.realized_pnl, Decimal::from(20));
        assert_eq!(view.realized_pnl, Decimal::from(20));

        fill(&mut portfolio, "B-2", "f4", &symbol, Side::Buy, 100, 2);
        mark(&mut portfolio, &symbol, 100);
        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::ONE);
        assert_eq!(position.avg_price.0, Decimal::from(100));
        assert_eq!(position.realized_pnl, Decimal::from(30));
        assert_eq!(view.realized_pnl, Decimal::from(30));
        assert_eq!(view.cash_balance, Decimal::from(930));
        assert_eq!(view.equity(), Decimal::from(1030));
    }

    #[test]
    fn long_cross_to_short_sets_residual_basis_to_fill_price() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let symbol = Symbol::new("SOLUSDT");

        fill(&mut portfolio, "B-1", "f1", &symbol, Side::Buy, 100, 1);
        fill(&mut portfolio, "S-1", "f2", &symbol, Side::Sell, 110, 2);
        mark(&mut portfolio, &symbol, 110);

        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::from(-1));
        assert_eq!(position.avg_price.0, Decimal::from(110));
        assert_eq!(view.realized_pnl, Decimal::from(10));
        assert_eq!(view.equity(), Decimal::from(1010));
    }

    #[test]
    fn duplicate_fill_is_idempotent() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let symbol = Symbol::new("BTCUSDT");

        fill(
            &mut portfolio,
            "B-1",
            "same-fill",
            &symbol,
            Side::Buy,
            100,
            1,
        );
        fill(
            &mut portfolio,
            "B-1",
            "newer-fill",
            &symbol,
            Side::Buy,
            120,
            1,
        );
        fill(
            &mut portfolio,
            "B-1",
            "same-fill",
            &symbol,
            Side::Buy,
            50,
            1,
        );

        let view = portfolio.reader().load();
        assert_eq!(view.positions[&symbol].quantity.0, Decimal::from(2));
        assert_eq!(view.cash_balance, Decimal::from(780));
        assert_eq!(view.unrealized_pnl, Decimal::from(20));
        assert_eq!(view.equity(), Decimal::from(1020));
    }

    #[test]
    fn marks_drive_equity_drawdown_from_initialized_capital() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let symbol = Symbol::new("BTCUSDT");
        fill(&mut portfolio, "B-1", "f1", &symbol, Side::Buy, 100, 1);

        mark(&mut portfolio, &symbol, 90);
        let losing = portfolio.reader().load();
        assert_eq!(losing.equity(), Decimal::from(990));
        assert_eq!(losing.high_water_mark, Decimal::from(1000));
        assert!((losing.drawdown_pct - 1.0).abs() < f64::EPSILON);
        assert!((losing.max_drawdown_pct - 1.0).abs() < f64::EPSILON);

        mark(&mut portfolio, &symbol, 110);
        let recovered = portfolio.reader().load();
        assert_eq!(recovered.equity(), Decimal::from(1010));
        assert_eq!(recovered.high_water_mark, Decimal::from(1010));
        assert_eq!(recovered.drawdown_pct, 0.0);
        assert!((recovered.max_drawdown_pct - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fill_and_fee_with_same_venue_id_use_separate_namespaces() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(1000));
        let order_id = OrderId("NAMESPACE-1".into());
        let symbol = Symbol::new("BTCUSDT");
        portfolio.register_order(order_id.clone(), symbol.clone(), Side::Buy);
        let fill = ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price(Decimal::from(100)),
            quantity: Quantity(Decimal::ONE),
            timestamp: 1,
            fill_id: "same-id".into(),
        };
        let fee = ExecutionEvent::FeeCharged {
            order_id,
            amount: Decimal::from(2),
            timestamp: 1,
            fill_id: "same-id".into(),
        };
        portfolio.on_execution_event(&fill);
        portfolio.on_execution_event(&fee);
        portfolio.on_execution_event(&fill);
        portfolio.on_execution_event(&fee);

        let view = portfolio.reader().load();
        assert_eq!(view.positions[&symbol].quantity.0, Decimal::ONE);
        assert_eq!(view.cash_balance, Decimal::from(898));
        assert_eq!(portfolio.export_state().processed_fill_ids.len(), 1);
        assert_eq!(portfolio.export_state().processed_fee_ids.len(), 1);
    }

    #[test]
    fn conflicting_order_metadata_is_retained_and_does_not_rebind_accounting() {
        let mut portfolio = Portfolio::new();
        let order_id = OrderId("META-CONFLICT".into());
        portfolio.register_order(order_id.clone(), Symbol::new("BTCUSDT"), Side::Buy);
        portfolio.register_order(order_id.clone(), Symbol::new("ETHUSDT"), Side::Sell);
        portfolio.on_execution_event(&ExecutionEvent::Fill {
            order_id,
            price: Price(Decimal::from(100)),
            quantity: Quantity(Decimal::ONE),
            timestamp: 1,
            fill_id: "metadata-fill".into(),
        });
        let state = portfolio.export_state();
        assert!(state
            .account_view
            .positions
            .contains_key(&Symbol::new("BTCUSDT")));
        assert!(!state
            .account_view
            .positions
            .contains_key(&Symbol::new("ETHUSDT")));
        assert_eq!(portfolio.reconciliation_exceptions().len(), 1);
    }

    #[test]
    fn invalid_snapshot_references_are_rejected_before_state_mutation() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(500));
        let mut state = portfolio.export_state();
        state
            .processed_fill_ids
            .insert(OrderId("UNKNOWN".into()), HashSet::from(["f".into()]));
        portfolio.import_state(state);
        assert_eq!(portfolio.reader().load().cash_balance, Decimal::from(500));
        assert_eq!(portfolio.reconciliation_exceptions().len(), 1);
    }

    #[test]
    fn financial_checkpoint_digest_rejects_tampered_cash_or_pnl() {
        let mut portfolio = Portfolio::with_cash_balance(Decimal::from(500));
        let mut state = portfolio.export_state();
        assert!(state.canonical_state_digest.is_some());
        state.account_view.cash_balance = Decimal::from(999);
        portfolio.import_state(state);
        assert_eq!(portfolio.reader().load().cash_balance, Decimal::from(500));
        assert_eq!(portfolio.reconciliation_exceptions().len(), 1);
    }

    #[test]
    fn financial_checkpoint_digest_rejects_cleared_reconciliation_evidence() {
        let mut portfolio = Portfolio::new();
        portfolio.on_execution_event(&ExecutionEvent::FeeCharged {
            order_id: OrderId("UNKNOWN-FEE".into()),
            amount: Decimal::ONE,
            timestamp: 1,
            fill_id: "fee-1".into(),
        });
        let mut state = portfolio.export_state();
        assert_eq!(state.reconciliation_exceptions.len(), 1);
        state.reconciliation_exceptions.clear();
        assert!(portfolio.try_import_state(state).is_err());
    }

    #[test]
    fn canonical_digest_is_stable_across_serialization_and_hash_insertion_order() {
        let mut portfolio = Portfolio::new();
        let order_a = OrderId("DIGEST-A".into());
        let order_b = OrderId("DIGEST-B".into());
        portfolio.register_order(order_a.clone(), Symbol::new("A"), Side::Buy);
        portfolio.register_order(order_b.clone(), Symbol::new("B"), Side::Buy);
        for (order_id, symbol, fill_id) in [
            (order_a.clone(), Symbol::new("A"), "a-1"),
            (order_a.clone(), Symbol::new("A"), "a-2"),
            (order_b.clone(), Symbol::new("B"), "b-1"),
        ] {
            portfolio.on_execution_event(&ExecutionEvent::Fill {
                order_id,
                price: Price(Decimal::ONE),
                quantity: Quantity(Decimal::ONE),
                timestamp: 1,
                fill_id: fill_id.into(),
            });
            let _ = symbol;
        }
        let state = portfolio.export_state();
        let serialized = serde_json::to_vec(&state).expect("serialize checkpoint");
        let round_tripped: PortfolioState =
            serde_json::from_slice(&serialized).expect("deserialize checkpoint");
        let mut restored = Portfolio::new();
        restored
            .try_import_state(round_tripped)
            .expect("serialized checkpoint restores");
        assert_eq!(
            restored.export_state().canonical_state_digest,
            state.canonical_state_digest
        );

        let mut reordered = state.clone();
        reordered.processed_fill_ids.clear();
        let mut reversed_a = HashSet::new();
        reversed_a.insert("a-2".to_string());
        reversed_a.insert("a-1".to_string());
        reordered
            .processed_fill_ids
            .insert(order_a.clone(), reversed_a);
        let mut reversed_b = HashSet::new();
        reversed_b.insert("b-1".to_string());
        reordered.processed_fill_ids.insert(order_b, reversed_b);
        reordered.canonical_state_digest = None;
        reordered.refresh_canonical_digest();
        assert_eq!(
            reordered.canonical_state_digest, state.canonical_state_digest,
            "HashMap/HashSet insertion order must not change the canonical digest"
        );
    }

    #[test]
    fn non_positive_fill_price_or_fee_is_rejected_with_evidence() {
        let mut portfolio = Portfolio::new();
        let order_id = OrderId("BAD-PRICE".into());
        let symbol = Symbol::new("BTCUSDT");
        portfolio.register_order(order_id.clone(), symbol, Side::Buy);
        portfolio.on_execution_event(&ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price(Decimal::ZERO),
            quantity: Quantity(Decimal::ONE),
            timestamp: 1,
            fill_id: "zero-price".into(),
        });
        portfolio.on_execution_event(&ExecutionEvent::FeeCharged {
            order_id,
            amount: Decimal::from(-1),
            timestamp: 1,
            fill_id: "negative-fee".into(),
        });
        assert!(portfolio.reader().load().positions.is_empty());
        assert_eq!(portfolio.reconciliation_exceptions().len(), 2);
    }
}
