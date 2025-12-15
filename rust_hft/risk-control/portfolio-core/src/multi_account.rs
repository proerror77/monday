//! 多帳戶 Portfolio 管理
//!
//! 支持跨交易所套利場景，提供：
//! - 按 venue/account 分別追蹤 PnL
//! - 聚合視圖 (總現金、總 PnL、總持倉)
//! - 跨帳戶回撤監控

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use hft_core::{OrderId, Price, Quantity, Side, Symbol, VenueId};
use ports::{AccountView, ExecutionEvent, Position};
use snapshot::SnapshotContainer;
use tracing::info;

/// 帳戶標識符（交易所 + 帳戶ID）
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountId {
    pub venue: VenueId,
    pub sub_account: Option<String>, // 子帳戶 ID（可選）
}

impl AccountId {
    pub fn new(venue: VenueId) -> Self {
        Self {
            venue,
            sub_account: None,
        }
    }

    pub fn with_sub_account(venue: VenueId, sub_account: String) -> Self {
        Self {
            venue,
            sub_account: Some(sub_account),
        }
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.sub_account {
            Some(sub) => write!(f, "{}:{}", self.venue.as_str(), sub),
            None => write!(f, "{}", self.venue.as_str()),
        }
    }
}

/// 聚合帳戶視圖（跨所總覽）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedAccountView {
    /// 各帳戶視圖
    pub accounts: HashMap<AccountId, AccountView>,
    /// 總現金餘額（跨所）
    pub total_cash_balance: Decimal,
    /// 總未實現盈虧
    pub total_unrealized_pnl: Decimal,
    /// 總已實現盈虧
    pub total_realized_pnl: Decimal,
    /// 總資產價值 (現金 + 未實現)
    pub total_equity: Decimal,
    /// 聚合持倉（相同 symbol 合併）
    pub aggregated_positions: HashMap<Symbol, AggregatedPosition>,
    /// 最大回撤（基於總資產）
    pub max_drawdown: Decimal,
    /// 最高水位線
    pub high_water_mark: Decimal,
    /// 最後更新時間
    pub last_update: u64,
}

/// 聚合持倉（跨所同一 symbol）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedPosition {
    pub symbol: Symbol,
    /// 淨持倉量（各所合計，可能對沖）
    pub net_quantity: Quantity,
    /// 加權平均價
    pub weighted_avg_price: Price,
    /// 各所分佈
    pub per_venue: HashMap<AccountId, Position>,
    /// 總未實現盈虧
    pub total_unrealized_pnl: Decimal,
}

impl Default for AggregatedAccountView {
    fn default() -> Self {
        Self {
            accounts: HashMap::new(),
            total_cash_balance: Decimal::ZERO,
            total_unrealized_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
            total_equity: Decimal::ZERO,
            aggregated_positions: HashMap::new(),
            max_drawdown: Decimal::ZERO,
            high_water_mark: Decimal::ZERO,
            last_update: 0,
        }
    }
}

/// 多帳戶 Portfolio State（持久化用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAccountState {
    /// 各帳戶狀態
    pub account_states: HashMap<AccountId, crate::PortfolioState>,
    /// 最高水位線
    pub high_water_mark: Decimal,
    /// 最大回撤
    pub max_drawdown: Decimal,
}

/// 多帳戶 Portfolio 管理器
pub struct MultiAccountPortfolio {
    /// 各帳戶的 Portfolio
    accounts: HashMap<AccountId, crate::Portfolio>,
    /// 訂單到帳戶的映射
    order_to_account: HashMap<OrderId, AccountId>,
    /// 聚合視圖快照
    aggregated_snapshot: SnapshotContainer<AggregatedAccountView>,
    /// 最高水位線
    high_water_mark: Decimal,
    /// 最大回撤
    max_drawdown: Decimal,
}

impl Default for MultiAccountPortfolio {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiAccountPortfolio {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            order_to_account: HashMap::new(),
            aggregated_snapshot: SnapshotContainer::new(AggregatedAccountView::default()),
            high_water_mark: Decimal::ZERO,
            max_drawdown: Decimal::ZERO,
        }
    }

    /// 添加/獲取帳戶
    pub fn get_or_create_account(&mut self, account_id: AccountId) -> &mut crate::Portfolio {
        self.accounts
            .entry(account_id)
            .or_insert_with(crate::Portfolio::new)
    }

    /// 獲取帳戶（只讀）
    pub fn get_account(&self, account_id: &AccountId) -> Option<&crate::Portfolio> {
        self.accounts.get(account_id)
    }

    /// 註冊訂單到特定帳戶
    pub fn register_order(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
        symbol: Symbol,
        side: Side,
    ) {
        self.order_to_account.insert(order_id.clone(), account_id.clone());
        let portfolio = self.get_or_create_account(account_id);
        portfolio.register_order(order_id, symbol, side);
    }

    /// 處理執行事件（自動路由到對應帳戶）
    pub fn on_execution_event(&mut self, event: &ExecutionEvent) {
        // 從事件中提取 order_id
        let order_id = match event {
            ExecutionEvent::OrderAck { order_id, .. } => order_id,
            ExecutionEvent::Fill { order_id, .. } => order_id,
            ExecutionEvent::OrderCanceled { order_id, .. } => order_id,
            ExecutionEvent::OrderReject { order_id, .. } => order_id,
            ExecutionEvent::OrderModified { order_id, .. } => order_id,
            _ => return,
        };

        // 根據訂單找到對應帳戶
        if let Some(account_id) = self.order_to_account.get(order_id).cloned() {
            if let Some(portfolio) = self.accounts.get_mut(&account_id) {
                portfolio.on_execution_event(event);
            }
        }

        // 更新聚合視圖
        self.update_aggregated_view();
    }

    /// 為特定帳戶處理執行事件（直接指定帳戶）
    pub fn on_execution_event_for_account(
        &mut self,
        account_id: &AccountId,
        event: &ExecutionEvent,
    ) {
        if let Some(portfolio) = self.accounts.get_mut(account_id) {
            portfolio.on_execution_event(event);
        }
        self.update_aggregated_view();
    }

    /// 更新特定帳戶的市場價格
    pub fn update_market_prices(
        &mut self,
        account_id: &AccountId,
        prices: &HashMap<Symbol, Price>,
    ) {
        if let Some(portfolio) = self.accounts.get_mut(account_id) {
            portfolio.update_market_prices(prices);
        }
        self.update_aggregated_view();
    }

    /// 更新所有帳戶的市場價格（同一 symbol 同價格）
    pub fn update_all_market_prices(&mut self, prices: &HashMap<Symbol, Price>) {
        for portfolio in self.accounts.values_mut() {
            portfolio.update_market_prices(prices);
        }
        self.update_aggregated_view();
    }

    /// 獲取聚合視圖讀取器
    pub fn aggregated_reader(&self) -> Arc<dyn snapshot::SnapshotReader<AggregatedAccountView>> {
        self.aggregated_snapshot.reader()
    }

    /// 獲取當前聚合視圖
    pub fn get_aggregated_view(&self) -> Arc<AggregatedAccountView> {
        self.aggregated_snapshot.reader().load()
    }

    /// 獲取單一帳戶視圖讀取器
    pub fn account_reader(
        &self,
        account_id: &AccountId,
    ) -> Option<Arc<dyn snapshot::SnapshotReader<AccountView>>> {
        self.accounts.get(account_id).map(|p| p.reader())
    }

    /// 更新聚合視圖
    fn update_aggregated_view(&mut self) {
        let mut agg = AggregatedAccountView::default();
        agg.last_update = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // 遍歷所有帳戶
        for (account_id, portfolio) in &self.accounts {
            let view = portfolio.reader().load();

            // 加入帳戶視圖
            agg.accounts.insert(account_id.clone(), (*view).clone());

            // 累加總額
            agg.total_cash_balance += view.cash_balance;
            agg.total_unrealized_pnl += view.unrealized_pnl;
            agg.total_realized_pnl += view.realized_pnl;

            // 聚合持倉
            for (symbol, position) in &view.positions {
                let agg_pos = agg
                    .aggregated_positions
                    .entry(symbol.clone())
                    .or_insert_with(|| AggregatedPosition {
                        symbol: symbol.clone(),
                        net_quantity: Quantity::zero(),
                        weighted_avg_price: Price::zero(),
                        per_venue: HashMap::new(),
                        total_unrealized_pnl: Decimal::ZERO,
                    });

                // 累加淨持倉
                agg_pos.net_quantity = Quantity(agg_pos.net_quantity.0 + position.quantity.0);
                agg_pos.total_unrealized_pnl += position.unrealized_pnl;
                agg_pos.per_venue.insert(account_id.clone(), position.clone());

                // 計算加權平均價（簡化：只累加，最後算）
                // weighted_avg = sum(qty * price) / sum(qty)
            }
        }

        // 計算加權平均價
        for agg_pos in agg.aggregated_positions.values_mut() {
            let mut total_value = Decimal::ZERO;
            let mut total_qty = Decimal::ZERO;

            for pos in agg_pos.per_venue.values() {
                if pos.quantity.0 > Decimal::ZERO {
                    total_value += pos.quantity.0 * pos.avg_price.0;
                    total_qty += pos.quantity.0;
                }
            }

            if total_qty > Decimal::ZERO {
                agg_pos.weighted_avg_price = Price(total_value / total_qty);
            }
        }

        // 計算總資產
        agg.total_equity = agg.total_cash_balance + agg.total_unrealized_pnl;

        // 更新最高水位線和回撤
        if agg.total_equity > self.high_water_mark {
            self.high_water_mark = agg.total_equity;
        }

        if self.high_water_mark > Decimal::ZERO {
            let current_dd = (self.high_water_mark - agg.total_equity) / self.high_water_mark;
            if current_dd > self.max_drawdown {
                self.max_drawdown = current_dd;
            }
        }

        agg.high_water_mark = self.high_water_mark;
        agg.max_drawdown = self.max_drawdown;

        // 發佈快照
        self.aggregated_snapshot.store(Arc::new(agg));
    }

    /// 導出狀態（持久化）
    pub fn export_state(&self) -> MultiAccountState {
        let mut account_states = HashMap::new();
        for (account_id, portfolio) in &self.accounts {
            account_states.insert(account_id.clone(), portfolio.export_state());
        }
        MultiAccountState {
            account_states,
            high_water_mark: self.high_water_mark,
            max_drawdown: self.max_drawdown,
        }
    }

    /// 導入狀態（恢復）
    pub fn import_state(&mut self, state: MultiAccountState) {
        info!(
            "Importing multi-account state - {} accounts, HWM: {}, DD: {}",
            state.account_states.len(),
            state.high_water_mark,
            state.max_drawdown
        );

        self.high_water_mark = state.high_water_mark;
        self.max_drawdown = state.max_drawdown;

        for (account_id, portfolio_state) in state.account_states {
            let portfolio = self.get_or_create_account(account_id.clone());
            portfolio.import_state(portfolio_state);
        }

        self.update_aggregated_view();

        info!(
            "Multi-account state imported - Total equity: {}",
            self.get_aggregated_view().total_equity
        );
    }

    /// 獲取跨帳戶 PnL 報告
    pub fn get_pnl_report(&self) -> PnlReport {
        let agg = self.get_aggregated_view();

        let mut per_account = HashMap::new();
        for (account_id, view) in &agg.accounts {
            per_account.insert(
                account_id.clone(),
                AccountPnl {
                    cash_balance: view.cash_balance,
                    unrealized_pnl: view.unrealized_pnl,
                    realized_pnl: view.realized_pnl,
                    total_pnl: view.unrealized_pnl + view.realized_pnl,
                    equity: view.cash_balance + view.unrealized_pnl,
                    position_count: view.positions.len(),
                },
            );
        }

        PnlReport {
            timestamp: agg.last_update,
            per_account,
            total_cash: agg.total_cash_balance,
            total_unrealized: agg.total_unrealized_pnl,
            total_realized: agg.total_realized_pnl,
            total_pnl: agg.total_unrealized_pnl + agg.total_realized_pnl,
            total_equity: agg.total_equity,
            high_water_mark: agg.high_water_mark,
            max_drawdown: agg.max_drawdown,
            current_drawdown: if agg.high_water_mark > Decimal::ZERO {
                (agg.high_water_mark - agg.total_equity) / agg.high_water_mark
            } else {
                Decimal::ZERO
            },
        }
    }
}

/// 帳戶 PnL 明細
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountPnl {
    pub cash_balance: Decimal,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub total_pnl: Decimal,
    pub equity: Decimal,
    pub position_count: usize,
}

/// 跨帳戶 PnL 報告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnlReport {
    pub timestamp: u64,
    pub per_account: HashMap<AccountId, AccountPnl>,
    pub total_cash: Decimal,
    pub total_unrealized: Decimal,
    pub total_realized: Decimal,
    pub total_pnl: Decimal,
    pub total_equity: Decimal,
    pub high_water_mark: Decimal,
    pub max_drawdown: Decimal,
    pub current_drawdown: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, Price, Quantity, VenueId};

    #[test]
    fn test_multi_account_basic() {
        let mut multi = MultiAccountPortfolio::new();

        // 創建兩個帳戶
        let binance = AccountId::new(VenueId::BINANCE);
        let bitget = AccountId::new(VenueId::BITGET);

        // 註冊訂單
        let sym = Symbol::new("BTCUSDT");
        multi.register_order(binance.clone(), OrderId("B-1".into()), sym.clone(), Side::Buy);
        multi.register_order(bitget.clone(), OrderId("G-1".into()), sym.clone(), Side::Sell);

        // 處理成交
        let fill_binance = ExecutionEvent::Fill {
            order_id: OrderId("B-1".into()),
            price: Price::from_f64(50000.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 1,
            fill_id: "f1".into(),
        };
        multi.on_execution_event(&fill_binance);

        let fill_bitget = ExecutionEvent::Fill {
            order_id: OrderId("G-1".into()),
            price: Price::from_f64(50100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 2,
            fill_id: "f2".into(),
        };
        multi.on_execution_event(&fill_bitget);

        // 檢查聚合視圖
        let agg = multi.get_aggregated_view();
        assert_eq!(agg.accounts.len(), 2);

        // 檢查 PnL 報告
        let report = multi.get_pnl_report();
        assert_eq!(report.per_account.len(), 2);
    }

    #[test]
    fn test_aggregated_position() {
        let mut multi = MultiAccountPortfolio::new();

        let binance = AccountId::new(VenueId::BINANCE);
        let bitget = AccountId::new(VenueId::BITGET);

        let sym = Symbol::new("ETHUSDT");

        // 兩所都買 ETH
        multi.register_order(binance.clone(), OrderId("B-1".into()), sym.clone(), Side::Buy);
        multi.register_order(bitget.clone(), OrderId("G-1".into()), sym.clone(), Side::Buy);

        multi.on_execution_event(&ExecutionEvent::Fill {
            order_id: OrderId("B-1".into()),
            price: Price::from_f64(3000.0).unwrap(),
            quantity: Quantity::from_f64(2.0).unwrap(),
            timestamp: 1,
            fill_id: "f1".into(),
        });

        multi.on_execution_event(&ExecutionEvent::Fill {
            order_id: OrderId("G-1".into()),
            price: Price::from_f64(3010.0).unwrap(),
            quantity: Quantity::from_f64(3.0).unwrap(),
            timestamp: 2,
            fill_id: "f2".into(),
        });

        let agg = multi.get_aggregated_view();

        // 檢查聚合持倉
        let eth_pos = agg.aggregated_positions.get(&sym).unwrap();
        assert_eq!(eth_pos.net_quantity.0, Decimal::from(5)); // 2 + 3
        assert_eq!(eth_pos.per_venue.len(), 2);

        // 加權平均價 = (2*3000 + 3*3010) / 5 = 15030 / 5 = 3006
        let expected_avg = Decimal::from(15030) / Decimal::from(5);
        assert_eq!(eth_pos.weighted_avg_price.0, expected_avg);
    }
}
