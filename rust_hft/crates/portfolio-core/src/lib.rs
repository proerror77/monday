//! Portfolio Core（會計真相源）
//! - 根據 ExecutionEvent（fills/fees/funding）更新帳戶狀態
//! - 發佈只讀 AccountView 快照（Arc 快照）

use std::collections::HashMap;
use std::sync::Arc;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use hft_core::{Price, Quantity, Symbol, Side, OrderId};
use ports::{AccountView, ExecutionEvent, Position};
use snapshot::SnapshotContainer;
use tracing::info;

/// Portfolio state that can be persisted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioState {
    pub account_view: AccountView,
    pub order_meta: HashMap<OrderId, (Symbol, Side)>,
    pub market_prices: HashMap<Symbol, Price>,
}

/// 最小 Portfolio：單帳戶，根據 fills 更新倉位/現金與 PnL
pub struct Portfolio {
    view: AccountView,
    snapshot: SnapshotContainer<AccountView>,
    // 供查詢：order_id → (symbol, side)
    order_meta: HashMap<hft_core::OrderId, (Symbol, Side)>,
    // 緩存最新市場價格用於 mark-to-market
    market_prices: HashMap<Symbol, Price>,
}

impl Portfolio {
    pub fn new() -> Self {
        let view = AccountView::default();
        let snapshot = SnapshotContainer::new(view.clone());
        Self { 
            view, 
            snapshot, 
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
        }
    }

    /// 註冊下單元資訊（供 fill 時查找 symbol/side）
    pub fn register_order(&mut self, order_id: hft_core::OrderId, symbol: Symbol, side: Side) {
        self.order_meta.insert(order_id, (symbol, side));
    }

    /// 處理執行事件，僅處理 Fill/Balance 類事件
    pub fn on_execution_event(&mut self, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::Fill { order_id, price, quantity, .. } => {
                if let Some((symbol, side)) = self.order_meta.get(order_id).cloned() {
                    self.apply_fill(&symbol, side, *price, *quantity);
                    // 更新該品種的市場價格為成交價（如果沒有更好的市場價格）
                    if !self.market_prices.contains_key(&symbol) {
                        self.market_prices.insert(symbol.clone(), *price);
                    }
                    // 重新計算未實現盈虧
                    self.recalculate_unrealized_pnl();
                }
            }
            _ => {}
        }
        // 每次更新後發佈只讀快照
        self.snapshot.store(Arc::new(self.view.clone()));
    }

    pub fn reader(&self) -> Arc<dyn snapshot::SnapshotReader<AccountView>> { self.snapshot.reader() }
    
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
        let mut total_unrealized = 0.0;
        
        for (symbol, position) in &mut self.view.positions {
            if let Some(market_price) = self.market_prices.get(symbol) {
                // 未實現盈虧 = (市場價 - 均價) * 持倉量
                // 注意：賣空倉位的 quantity 為負數
                let unrealized = (market_price.0 - position.avg_price.0) * position.quantity.0;
                position.unrealized_pnl = unrealized.to_f64().unwrap_or(0.0);
                total_unrealized += position.unrealized_pnl;
            }
        }
        
        self.view.unrealized_pnl = total_unrealized;
    }

    fn apply_fill(&mut self, symbol: &Symbol, side: Side, price: Price, qty: Quantity) {
        let pos = self.view.positions.entry(symbol.clone()).or_insert(Position {
            symbol: symbol.clone(),
            quantity: Quantity::zero(),
            avg_price: Price::zero(),
            unrealized_pnl: 0.0,
        });

        let qty_f = qty.to_f64().unwrap_or(0.0);
        let price_f = price.to_f64().unwrap_or(0.0);

        match side {
            Side::Buy => {
                // 新均價 = (舊倉*舊均價 + 新買*成交價) / 新總倉
                let old_qty = pos.quantity.0;
                let new_qty = Quantity(old_qty + qty.0);
                let new_avg = if new_qty.0 > rust_decimal::Decimal::ZERO {
                    Price((pos.avg_price.0 * old_qty + price.0 * qty.0) / new_qty.0)
                } else { price };
                pos.quantity = new_qty;
                pos.avg_price = new_avg;
                // 現金減少
                self.view.cash_balance -= price_f * qty_f;
            }
            Side::Sell => {
                // 實現損益 = (賣價 - 均價) * 賣出數量
                let realized = (price.0 - pos.avg_price.0) * qty.0;
                self.view.realized_pnl += realized.to_f64().unwrap_or(0.0);
                // 減倉
                pos.quantity = Quantity(pos.quantity.0 - qty.0);
                // 現金增加
                self.view.cash_balance += price_f * qty_f;
            }
        }
    }
    
    /// Export portfolio state for persistence
    pub fn export_state(&self) -> PortfolioState {
        PortfolioState {
            account_view: self.view.clone(),
            order_meta: self.order_meta.clone(),
            market_prices: self.market_prices.clone(),
        }
    }
    
    /// Import portfolio state from persistent storage
    pub fn import_state(&mut self, state: PortfolioState) {
        
        info!("Importing portfolio state - Cash: {:.2}, Positions: {}, Orders: {}", 
              state.account_view.cash_balance,
              state.account_view.positions.len(),
              state.order_meta.len());
        
        self.view = state.account_view;
        self.order_meta = state.order_meta;
        self.market_prices = state.market_prices;
        
        // Recalculate unrealized PnL with current market prices
        self.recalculate_unrealized_pnl();
        
        // Update snapshot
        self.snapshot.store(Arc::new(self.view.clone()));
        
        // Log summary
        info!("Portfolio state imported - Total value: {:.2}, Realized PnL: {:.2}, Unrealized PnL: {:.2}",
              self.view.cash_balance + self.view.unrealized_pnl,
              self.view.realized_pnl,
              self.view.unrealized_pnl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, Price, Quantity};

    #[test]
    fn test_portfolio_fill_updates() {
        let mut pf = Portfolio::new();
        let oid = OrderId("O-1".into());
        let sym = Symbol("BTCUSDT".into());
        pf.register_order(oid.clone(), sym.clone(), Side::Buy);

        let ev = ExecutionEvent::Fill { order_id: oid, price: Price::from_f64(100.0).unwrap(), quantity: Quantity::from_f64(1.0).unwrap(), timestamp: 0, fill_id: "f1".into() };
        pf.on_execution_event(&ev);

        let view = pf.reader().load();
        assert!(view.positions.get(&sym).is_some());
    }
}
