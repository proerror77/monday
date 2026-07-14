//! Portfolio Core（會計真相源）
//! - 根據 ExecutionEvent（fills/fees/funding）更新帳戶狀態
//! - 發佈只讀 AccountView 快照（Arc 快照）
//! - 多帳戶 PnL 聚合（跨交易所）

pub mod multi_account;

pub use multi_account::{
    AccountId, AccountPnl, AggregatedAccountView, AggregatedPosition, MultiAccountPortfolio,
    MultiAccountState, PnlReport,
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hft_core::{OrderId, Price, Quantity, Side, Symbol};
use ports::{AccountView, ExecutionEvent, Position};
use snapshot::SnapshotContainer;
use tracing::{info, warn};

/// Portfolio state that can be persisted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioState {
    pub account_view: AccountView,
    pub order_meta: HashMap<OrderId, (Symbol, Side)>,
    pub market_prices: HashMap<Symbol, Price>,
    /// 已處理的成交ID（去重），恢復後避免重覆累計
    #[serde(default)]
    pub processed_fill_ids: HashMap<OrderId, HashSet<String>>,
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
        self.order_meta.insert(order_id, (symbol, side));
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
                    return;
                }
                if let Some((symbol, side)) = self.order_meta.get(order_id).cloned() {
                    // De-duplication: skip duplicated fill_id for this order
                    let set = self.processed_fill_ids.entry(order_id.clone()).or_default();
                    if fill_id.is_empty() || !set.contains(fill_id) {
                        if !fill_id.is_empty() {
                            set.insert(fill_id.clone());
                        }
                        self.apply_fill(&symbol, side, *price, *quantity);
                        // Fill price is the freshest executable fallback mark observed by this ledger.
                        self.market_prices.insert(symbol.clone(), *price);
                        self.recalculate_unrealized_pnl();
                    }
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
                    return;
                }
                if self.order_meta.contains_key(order_id) {
                    let fee_id = format!("fee:{fill_id}");
                    let set = self.processed_fill_ids.entry(order_id.clone()).or_default();
                    if set.insert(fee_id) {
                        self.view.cash_balance -= *amount;
                        self.view.realized_pnl -= *amount;
                        self.update_drawdown_stats();
                    }
                } else {
                    warn!(order_id = %order_id.0, "ignoring fee for unknown order");
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
            self.view.realized_pnl += (price.0 - pos.avg_price.0) * closed_quantity * direction;

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
        PortfolioState {
            account_view: self.view.clone(),
            order_meta: self.order_meta.clone(),
            market_prices: self.market_prices.clone(),
            processed_fill_ids: self.processed_fill_ids.clone(),
        }
    }

    /// Import portfolio state from persistent storage
    pub fn import_state(&mut self, state: PortfolioState) {
        info!(
            "Importing portfolio state - Cash: {}, Positions: {}, Orders: {}",
            state.account_view.cash_balance,
            state.account_view.positions.len(),
            state.order_meta.len()
        );

        self.view = state.account_view;
        self.order_meta = state.order_meta;
        self.market_prices = state.market_prices;
        self.processed_fill_ids = state.processed_fill_ids;

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
    }
}

/// 實現 PortfolioManager trait - 將現有方法適配為 trait 接口
impl ports::PortfolioManager for Portfolio {
    fn register_order(&mut self, order_id: hft_core::OrderId, symbol: Symbol, side: Side) {
        // 直接調用現有實現
        self.register_order(order_id, symbol, side);
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
            order_meta: internal_state.order_meta,
            market_prices: internal_state.market_prices,
            processed_fill_ids: internal_state.processed_fill_ids,
        }
    }

    fn import_state(&mut self, state: ports::PortfolioState) {
        // 轉換 ports::PortfolioState 為內部 PortfolioState
        let internal_state = PortfolioState {
            account_view: state.account_view,
            order_meta: state.order_meta,
            market_prices: state.market_prices,
            processed_fill_ids: state.processed_fill_ids,
        };
        self.import_state(internal_state);
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
        assert_eq!(view.realized_pnl, Decimal::from(20));

        fill(&mut portfolio, "B-2", "f4", &symbol, Side::Buy, 100, 2);
        mark(&mut portfolio, &symbol, 100);
        let view = portfolio.reader().load();
        let position = view.positions.get(&symbol).unwrap();
        assert_eq!(position.quantity.0, Decimal::ONE);
        assert_eq!(position.avg_price.0, Decimal::from(100));
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
}
