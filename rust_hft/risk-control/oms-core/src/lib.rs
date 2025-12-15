//! OMS Core（純邏輯）
//! - 訂單狀態機（Ack/Partial/Fill/Cancel/Rejected/Expired/Replaced）
//! - 冪等與路由策略（不含任何網路）

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use hft_core::{OrderId, Price, Quantity, Side, Symbol};
use ports::ExecutionEvent;
use tracing::{debug, info};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    New,
    Acknowledged,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
    Replaced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRecord {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub status: OrderStatus,
    /// Optional venue where this order is routed to
    pub venue: Option<hft_core::VenueId>,
    /// Optional strategy id that created this order
    pub strategy_id: Option<String>,
    /// Processed fill ids for de-duplication
    pub processed_fill_ids: HashSet<String>,
}

/// Parameters for registering a new order
#[derive(Debug, Clone)]
pub struct RegisterOrderParams {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub venue: Option<hft_core::VenueId>,
    pub strategy_id: Option<String>,
}

impl OrderRecord {
    fn new(params: RegisterOrderParams) -> Self {
        Self {
            order_id: params.order_id,
            client_order_id: params.client_order_id,
            symbol: params.symbol,
            side: params.side,
            qty: params.qty,
            cum_qty: Quantity::zero(),
            avg_price: None,
            status: OrderStatus::New,
            venue: params.venue,
            strategy_id: params.strategy_id,
            processed_fill_ids: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdate {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub previous_status: OrderStatus,
}

/// 最小 OMS 實作：維護 order_id → 訂單資訊 與 狀態機
#[derive(Default)]
pub struct OmsCore {
    orders: HashMap<OrderId, OrderRecord>,
}

impl OmsCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 註冊新下單（由引擎在 place_order 成功前後調用）
    pub fn register_order(&mut self, params: RegisterOrderParams) {
        let order_id = params.order_id.clone();
        self.orders.insert(order_id, OrderRecord::new(params));
    }

    /// 應用執行事件（私有 WS 回報）更新狀態機
    /// 返回狀態更新，如果狀態變化為 Filled 則會在引擎層觸發 OrderCompleted 事件
    pub fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<OrderUpdate> {
        match event {
            ExecutionEvent::OrderAck { order_id, .. } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    ord.status = OrderStatus::Acknowledged;
                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::Fill {
                order_id,
                price,
                quantity,
                fill_id,
                ..
            } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    // De-duplication: skip duplicate fills by fill_id
                    if !fill_id.is_empty() {
                        if ord.processed_fill_ids.contains(fill_id) {
                            debug!(
                                "Duplicate fill ignored: order_id={}, fill_id={}",
                                order_id.0, fill_id
                            );
                            return None;
                        } else {
                            ord.processed_fill_ids.insert(fill_id.clone());
                        }
                    }
                    // 精度保護：使用 Decimal 進行所有計算，避免浮點中間態
                    let prev_cum_qty = ord.cum_qty.0;
                    let fill_qty = quantity.0;
                    let new_cum_qty = prev_cum_qty + fill_qty;

                    ord.cum_qty = Quantity(new_cum_qty);

                    // 狀態轉換：根據累計成交量判斷是否完全成交
                    let previous_status = ord.status;
                    ord.status = if new_cum_qty >= ord.qty.0 {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    };

                    // 精確的加權平均價格計算 (全 Decimal，避免 f64 中間態)
                    ord.avg_price = Some(match ord.avg_price {
                        None => *price,
                        Some(prev_avg) => {
                            if new_cum_qty > rust_decimal::Decimal::ZERO {
                                // 加權平均：(prev_avg * prev_qty + fill_price * fill_qty) / total_qty
                                let weighted_prev = prev_avg.0 * prev_cum_qty;
                                let weighted_fill = price.0 * fill_qty;
                                Price((weighted_prev + weighted_fill) / new_cum_qty)
                            } else {
                                *price
                            }
                        }
                    });

                    // 記錄狀態變化日誌
                    if previous_status != ord.status {
                        if ord.status == OrderStatus::Filled {
                            info!("訂單完全成交: order_id={}, cum_qty={}, total_qty={}, avg_price={:?}",
                                  order_id.0, new_cum_qty, ord.qty.0, ord.avg_price);
                        } else {
                            debug!(
                                "訂單部分成交: order_id={}, cum_qty={}/{}, avg_price={:?}",
                                order_id.0, new_cum_qty, ord.qty.0, ord.avg_price
                            );
                        }
                    }

                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::OrderReject { order_id, .. } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    ord.status = OrderStatus::Rejected;
                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::OrderCanceled { order_id, .. } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    ord.status = OrderStatus::Canceled;
                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::OrderModified {
                order_id,
                new_quantity,
                ..
            } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    // 更新訂單數量和價格 (如果提供)
                    if let Some(new_qty) = new_quantity {
                        ord.qty = *new_qty;
                        // 重新檢查是否應該變為已成交狀態
                        ord.status = if ord.cum_qty.0 >= ord.qty.0 {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    }
                    // 注意：修改價格不會影響已有的平均成交價
                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::OrderCompleted { .. } => {
                // OrderCompleted 是由引擎層基於 OMS 狀態變化生成的，這裡不處理避免循環
                debug!("接收到 OrderCompleted 事件，OMS 層忽略處理");
            }
            _ => {
                debug!("OMS 層忽略未處理的執行事件類型");
            }
        }
        None
    }

    pub fn get(&self, id: &OrderId) -> Option<&OrderRecord> {
        self.orders.get(id)
    }

    /// 取得指定策略的未結訂單（完整記錄）
    pub fn get_open_orders_by_strategy(&self, strategy_id: &str) -> Vec<&OrderRecord> {
        self.orders
            .values()
            .filter(|order| {
                matches!(
                    order.status,
                    OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
                ) && order.strategy_id.as_deref() == Some(strategy_id)
            })
            .collect()
    }

    /// 取得指定策略的未結訂單 (order_id, symbol) 配對，方便批量撤單
    pub fn open_order_pairs_by_strategy(&self, strategy_id: &str) -> Vec<(OrderId, Symbol)> {
        self.orders
            .iter()
            .filter_map(|(oid, rec)| {
                if matches!(
                    rec.status,
                    OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
                ) && rec.strategy_id.as_deref() == Some(strategy_id)
                {
                    Some((oid.clone(), rec.symbol.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// 各策略未結訂單數量統計
    pub fn open_counts_by_strategy(&self) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for rec in self.orders.values() {
            if matches!(
                rec.status,
                OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
            ) {
                if let Some(sid) = &rec.strategy_id {
                    *counts.entry(sid.clone()).or_insert(0) += 1;
                }
            }
        }
        counts
    }

    /// Export OMS state for persistence
    pub fn export_state(&self) -> HashMap<OrderId, OrderRecord> {
        self.orders.clone()
    }

    /// Import OMS state from persistent storage
    pub fn import_state(&mut self, state: HashMap<OrderId, OrderRecord>) {
        info!("Importing OMS state with {} orders", state.len());
        self.orders = state;

        // Log summary of imported orders
        let mut status_counts = HashMap::new();
        for record in self.orders.values() {
            *status_counts.entry(record.status).or_insert(0) += 1;
        }

        info!("OMS state imported - Status summary: {:?}", status_counts);
    }

    /// Get all open orders (not Filled, Canceled, Rejected, or Expired)
    pub fn get_open_orders(&self) -> Vec<&OrderRecord> {
        self.orders
            .values()
            .filter(|order| {
                matches!(
                    order.status,
                    OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
                )
            })
            .collect()
    }

    /// Get total number of orders
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }

    /// 直接更新某筆訂單的累計成交量與平均成交價（恢復/對賬使用）
    pub fn update_filled_quantity(
        &mut self,
        order_id: &OrderId,
        new_cum_qty: Quantity,
        new_avg_price: Option<Price>,
    ) -> Option<OrderUpdate> {
        if let Some(ord) = self.orders.get_mut(order_id) {
            let previous_status = ord.status;
            ord.cum_qty = new_cum_qty;
            if let Some(px) = new_avg_price {
                ord.avg_price = Some(px);
            }
            // 根據累計成交量與原始數量推導狀態
            ord.status = if ord.cum_qty.0 >= ord.qty.0 {
                OrderStatus::Filled
            } else if ord.cum_qty.0 > rust_decimal::Decimal::ZERO {
                OrderStatus::PartiallyFilled
            } else {
                ord.status
            };
            return Some(OrderUpdate {
                order_id: order_id.clone(),
                status: ord.status,
                cum_qty: ord.cum_qty,
                avg_price: ord.avg_price,
                previous_status,
            });
        }
        None
    }

    /// 直接更新某筆訂單的狀態（恢復/對賬使用）
    pub fn update_status(
        &mut self,
        order_id: &OrderId,
        new_status: OrderStatus,
    ) -> Option<OrderUpdate> {
        if let Some(ord) = self.orders.get_mut(order_id) {
            let previous_status = ord.status;
            ord.status = new_status;
            return Some(OrderUpdate {
                order_id: order_id.clone(),
                status: ord.status,
                cum_qty: ord.cum_qty,
                avg_price: ord.avg_price,
                previous_status,
            });
        }
        None
    }

    /// 對帳：比較交易所未結訂單與本地 OMS 狀態，返回差異報告
    ///
    /// Returns:
    /// - exchange_only: 訂單在交易所存在但本地未追蹤
    /// - local_only: 訂單在本地存在但交易所沒有（可能已被撤銷）
    /// - qty_mismatch: 訂單存在但成交量不一致
    pub fn reconcile_with_exchange(
        &self,
        exchange_orders: &[ports::OpenOrder],
    ) -> ReconciliationReport {
        let mut report = ReconciliationReport::default();

        // Build a set of exchange order IDs for quick lookup
        let exchange_ids: HashSet<_> = exchange_orders.iter().map(|o| &o.order_id).collect();

        // Check for exchange-only and quantity mismatches
        for ex_order in exchange_orders {
            if let Some(local_order) = self.orders.get(&ex_order.order_id) {
                // Order exists in both - check for quantity mismatch
                let exchange_filled = ex_order.filled_quantity;
                let local_filled = local_order.cum_qty;
                if exchange_filled != local_filled {
                    report.qty_mismatch.push(QuantityMismatch {
                        order_id: ex_order.order_id.clone(),
                        symbol: ex_order.symbol.clone(),
                        exchange_filled,
                        local_filled,
                    });
                }
            } else {
                // Order exists on exchange but not locally
                report.exchange_only.push(ex_order.order_id.clone());
            }
        }

        // Check for local-only orders (orders we think are open but exchange doesn't have)
        for (order_id, record) in &self.orders {
            if matches!(
                record.status,
                OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
            ) && !exchange_ids.contains(order_id)
            {
                report.local_only.push(LocalOnlyOrder {
                    order_id: order_id.clone(),
                    symbol: record.symbol.clone(),
                    status: record.status,
                });
            }
        }

        report
    }
}

/// 對帳報告
#[derive(Debug, Clone, Default)]
pub struct ReconciliationReport {
    /// 訂單在交易所存在但本地未追蹤
    pub exchange_only: Vec<OrderId>,
    /// 訂單在本地存在但交易所沒有
    pub local_only: Vec<LocalOnlyOrder>,
    /// 訂單存在但成交量不一致
    pub qty_mismatch: Vec<QuantityMismatch>,
}

impl ReconciliationReport {
    /// 是否有差異需要處理
    pub fn has_discrepancies(&self) -> bool {
        !self.exchange_only.is_empty()
            || !self.local_only.is_empty()
            || !self.qty_mismatch.is_empty()
    }

    /// 總差異數量
    pub fn total_discrepancies(&self) -> usize {
        self.exchange_only.len() + self.local_only.len() + self.qty_mismatch.len()
    }
}

/// 本地獨有訂單
#[derive(Debug, Clone)]
pub struct LocalOnlyOrder {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub status: OrderStatus,
}

/// 成交量不一致
#[derive(Debug, Clone)]
pub struct QuantityMismatch {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub exchange_filled: Quantity,
    pub local_filled: Quantity,
}

// 實現 OrderManager trait
impl ports::OrderManager for OmsCore {
    fn register_order(&mut self, params: ports::RegisterOrderParams) {
        self.register_order(RegisterOrderParams {
            order_id: params.order_id,
            client_order_id: params.client_order_id,
            symbol: params.symbol,
            side: params.side,
            qty: params.qty,
            venue: params.venue,
            strategy_id: params.strategy_id,
        });
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<ports::OrderUpdate> {
        self.on_execution_event(event)
            .map(|update| ports::OrderUpdate {
                order_id: update.order_id,
                status: match update.status {
                    OrderStatus::New => ports::OrderStatus::New,
                    OrderStatus::Acknowledged => ports::OrderStatus::Acknowledged,
                    OrderStatus::PartiallyFilled => ports::OrderStatus::PartiallyFilled,
                    OrderStatus::Filled => ports::OrderStatus::Filled,
                    OrderStatus::Canceled => ports::OrderStatus::Canceled,
                    OrderStatus::Rejected => ports::OrderStatus::Rejected,
                    OrderStatus::Expired => ports::OrderStatus::Expired,
                    OrderStatus::Replaced => ports::OrderStatus::Replaced,
                },
                cum_qty: update.cum_qty,
                avg_price: update.avg_price,
                previous_status: match update.previous_status {
                    OrderStatus::New => ports::OrderStatus::New,
                    OrderStatus::Acknowledged => ports::OrderStatus::Acknowledged,
                    OrderStatus::PartiallyFilled => ports::OrderStatus::PartiallyFilled,
                    OrderStatus::Filled => ports::OrderStatus::Filled,
                    OrderStatus::Canceled => ports::OrderStatus::Canceled,
                    OrderStatus::Rejected => ports::OrderStatus::Rejected,
                    OrderStatus::Expired => ports::OrderStatus::Expired,
                    OrderStatus::Replaced => ports::OrderStatus::Replaced,
                },
            })
    }

    fn export_state(&self) -> std::collections::HashMap<OrderId, ports::OrderRecord> {
        self.orders
            .iter()
            .map(|(id, rec)| {
                (
                    id.clone(),
                    ports::OrderRecord {
                        order_id: rec.order_id.clone(),
                        client_order_id: rec.client_order_id.clone(),
                        symbol: rec.symbol.clone(),
                        side: rec.side,
                        qty: rec.qty,
                        cum_qty: rec.cum_qty,
                        avg_price: rec.avg_price,
                        status: match rec.status {
                            OrderStatus::New => ports::OrderStatus::New,
                            OrderStatus::Acknowledged => ports::OrderStatus::Acknowledged,
                            OrderStatus::PartiallyFilled => ports::OrderStatus::PartiallyFilled,
                            OrderStatus::Filled => ports::OrderStatus::Filled,
                            OrderStatus::Canceled => ports::OrderStatus::Canceled,
                            OrderStatus::Rejected => ports::OrderStatus::Rejected,
                            OrderStatus::Expired => ports::OrderStatus::Expired,
                            OrderStatus::Replaced => ports::OrderStatus::Replaced,
                        },
                        venue: rec.venue,
                        strategy_id: rec.strategy_id.clone(),
                    },
                )
            })
            .collect()
    }

    fn import_state(&mut self, state: std::collections::HashMap<OrderId, ports::OrderRecord>) {
        let converted_state: HashMap<OrderId, OrderRecord> = state
            .into_iter()
            .map(|(id, rec)| {
                (
                    id,
                    OrderRecord {
                        order_id: rec.order_id,
                        client_order_id: rec.client_order_id,
                        symbol: rec.symbol,
                        side: rec.side,
                        qty: rec.qty,
                        cum_qty: rec.cum_qty,
                        avg_price: rec.avg_price,
                        status: match rec.status {
                            ports::OrderStatus::New => OrderStatus::New,
                            ports::OrderStatus::Acknowledged => OrderStatus::Acknowledged,
                            ports::OrderStatus::Accepted => OrderStatus::Acknowledged, // 映射 Accepted 為 Acknowledged
                            ports::OrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
                            ports::OrderStatus::Filled => OrderStatus::Filled,
                            ports::OrderStatus::Canceled => OrderStatus::Canceled,
                            ports::OrderStatus::Rejected => OrderStatus::Rejected,
                            ports::OrderStatus::Expired => OrderStatus::Expired,
                            ports::OrderStatus::Replaced => OrderStatus::Replaced,
                        },
                        venue: rec.venue,
                        strategy_id: rec.strategy_id,
                        processed_fill_ids: HashSet::new(), // 恢復時重置去重集合
                    },
                )
            })
            .collect();
        self.import_state(converted_state);
    }

    fn open_order_pairs_by_strategy(&self, strategy_id: &str) -> Vec<(OrderId, Symbol)> {
        self.open_order_pairs_by_strategy(strategy_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, Price, Quantity, Symbol};

    #[test]
    fn test_ack_and_fill() {
        let mut oms = OmsCore::new();
        let oid = OrderId("T-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("test_strategy".to_string()),
        });

        let ack = ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let up = oms.on_execution_event(&ack).unwrap();
        assert_eq!(up.status, OrderStatus::Acknowledged);

        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.4).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let up2 = oms.on_execution_event(&fill).unwrap();
        assert_eq!(up2.status, OrderStatus::PartiallyFilled);
    }

    #[test]
    fn test_open_counts_by_strategy() {
        let mut oms = OmsCore::new();
        // o1: New (open)
        let o1 = OrderId("S-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: o1.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("stratA".into()),
        });
        // o2: Ack (open)
        let o2 = OrderId("S-2".into());
        oms.register_order(RegisterOrderParams {
            order_id: o2.clone(),
            client_order_id: None,
            symbol: Symbol::new("ETHUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: None,
            strategy_id: Some("stratA".into()),
        });
        let _ = oms.on_execution_event(&ExecutionEvent::OrderAck {
            order_id: o2.clone(),
            timestamp: 0,
        });
        // o3: Filled (not open)
        let o3 = OrderId("S-3".into());
        oms.register_order(RegisterOrderParams {
            order_id: o3.clone(),
            client_order_id: None,
            symbol: Symbol::new("SOLUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("stratB".into()),
        });
        let _ = oms.on_execution_event(&ExecutionEvent::Fill {
            order_id: o3,
            price: Price::from_f64(10.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 0,
            fill_id: "x".into(),
        });

        let counts = oms.open_counts_by_strategy();
        assert_eq!(counts.get("stratA"), Some(&2));
        assert!(!counts.contains_key("stratB"));
    }

    #[test]
    fn test_update_filled_quantity_reconciliation() {
        let mut oms = OmsCore::new();
        let oid = OrderId("RECON-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: None,
            strategy_id: Some("test".into()),
        });

        // Simulate OMS has 0.5 filled, but exchange says 1.5 filled
        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.5).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Reconciliation: update to exchange's truth
        let update = oms
            .update_filled_quantity(
                &oid,
                Quantity::from_f64(1.5).unwrap(),
                Some(Price::from_f64(101.0).unwrap()),
            )
            .unwrap();

        assert_eq!(update.status, OrderStatus::PartiallyFilled);
        assert_eq!(update.cum_qty, Quantity::from_f64(1.5).unwrap());
        assert_eq!(update.avg_price, Some(Price::from_f64(101.0).unwrap()));
    }

    #[test]
    fn test_update_status_reconciliation() {
        let mut oms = OmsCore::new();
        let oid = OrderId("RECON-2".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("ETHUSDT"),
            side: Side::Sell,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("test".into()),
        });

        // Order is New locally, but exchange says it's Canceled
        let update = oms.update_status(&oid, OrderStatus::Canceled).unwrap();
        assert_eq!(update.previous_status, OrderStatus::New);
        assert_eq!(update.status, OrderStatus::Canceled);

        // Verify the order is no longer in open orders
        let open = oms.get_open_orders();
        assert!(open.is_empty());
    }

    #[test]
    fn test_duplicate_fill_deduplication() {
        let mut oms = OmsCore::new();
        let oid = OrderId("DEDUP-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.3).unwrap(),
            timestamp: 0,
            fill_id: "fill-123".into(),
        };

        // First fill should be processed
        let result1 = oms.on_execution_event(&fill);
        assert!(result1.is_some());
        assert_eq!(result1.unwrap().cum_qty, Quantity::from_f64(0.3).unwrap());

        // Duplicate fill with same fill_id should be ignored
        let result2 = oms.on_execution_event(&fill);
        assert!(result2.is_none());

        // Verify cum_qty didn't change
        let order = oms.get(&oid).unwrap();
        assert_eq!(order.cum_qty, Quantity::from_f64(0.3).unwrap());
    }

    #[test]
    fn test_full_fill_completes_order() {
        let mut oms = OmsCore::new();
        let oid = OrderId("FULL-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // Partial fill
        let fill1 = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.6).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let r1 = oms.on_execution_event(&fill1).unwrap();
        assert_eq!(r1.status, OrderStatus::PartiallyFilled);

        // Complete fill
        let fill2 = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(101.0).unwrap(),
            quantity: Quantity::from_f64(0.4).unwrap(),
            timestamp: 0,
            fill_id: "f2".into(),
        };
        let r2 = oms.on_execution_event(&fill2).unwrap();
        assert_eq!(r2.status, OrderStatus::Filled);

        // Verify order is no longer in open orders
        let open = oms.get_open_orders();
        assert!(open.is_empty());
    }

    #[test]
    fn test_export_import_state() {
        let mut oms = OmsCore::new();
        let oid = OrderId("EXP-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: Some("client-1".into()),
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: Some(hft_core::VenueId::BITGET),
            strategy_id: Some("strat-a".into()),
        });

        // Add some fills
        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.5).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Export state
        let state = oms.export_state();
        assert_eq!(state.len(), 1);

        // Import to new OMS
        let mut oms2 = OmsCore::new();
        oms2.import_state(state);

        // Verify state is preserved
        let order = oms2.get(&oid).unwrap();
        assert_eq!(order.client_order_id, Some("client-1".into()));
        assert_eq!(order.cum_qty, Quantity::from_f64(0.5).unwrap());
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        assert_eq!(order.venue, Some(hft_core::VenueId::BITGET));
        assert_eq!(order.strategy_id, Some("strat-a".into()));
    }

    #[test]
    fn test_order_rejected_removes_from_open() {
        let mut oms = OmsCore::new();
        let oid = OrderId("REJ-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("test".into()),
        });

        // Verify order is in open orders
        assert_eq!(oms.get_open_orders().len(), 1);

        // Reject the order
        let reject = ExecutionEvent::OrderReject {
            order_id: oid.clone(),
            reason: "Insufficient funds".into(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&reject);

        // Verify order is no longer in open orders
        assert!(oms.get_open_orders().is_empty());
    }

    #[test]
    fn test_order_canceled_removes_from_open() {
        let mut oms = OmsCore::new();
        let oid = OrderId("CAN-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: Some("test".into()),
        });

        // ACK first
        let ack = ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&ack);

        // Verify order is in open orders
        assert_eq!(oms.get_open_orders().len(), 1);

        // Cancel
        let cancel = ExecutionEvent::OrderCanceled {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&cancel);

        // Verify order is no longer in open orders
        assert!(oms.get_open_orders().is_empty());
    }

    #[test]
    fn test_weighted_avg_price_calculation() {
        let mut oms = OmsCore::new();
        let oid = OrderId("AVG-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // Fill 0.4 at 100
        let fill1 = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.4).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill1);

        // Fill 0.6 at 110
        let fill2 = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(110.0).unwrap(),
            quantity: Quantity::from_f64(0.6).unwrap(),
            timestamp: 0,
            fill_id: "f2".into(),
        };
        let _ = oms.on_execution_event(&fill2);

        // Expected avg: (0.4*100 + 0.6*110) / 1.0 = 106
        let order = oms.get(&oid).unwrap();
        let avg = order.avg_price.unwrap().0;
        let expected = rust_decimal::Decimal::from(106);
        let tolerance = rust_decimal::Decimal::new(1, 3); // 0.001
        assert!(
            (avg - expected).abs() < tolerance,
            "Expected avg_price ~106, got {}",
            avg
        );
    }

    #[test]
    fn test_order_modify_updates_quantity() {
        let mut oms = OmsCore::new();
        let oid = OrderId("MOD-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // ACK
        let ack = ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&ack);

        // Partial fill
        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.5).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Modify to reduce quantity to 0.5 (matching filled amount)
        let modify = ExecutionEvent::OrderModified {
            order_id: oid.clone(),
            new_quantity: Some(Quantity::from_f64(0.5).unwrap()),
            new_price: None,
            timestamp: 0,
        };
        let update = oms.on_execution_event(&modify).unwrap();

        // Order should now be Filled since cum_qty >= qty
        assert_eq!(update.status, OrderStatus::Filled);
    }

    fn create_exchange_order(id: &str, symbol: &str, filled: f64) -> ports::OpenOrder {
        ports::OpenOrder {
            order_id: OrderId(id.into()),
            symbol: Symbol::new(symbol),
            side: Side::Buy,
            order_type: hft_core::OrderType::Limit,
            original_quantity: Quantity::from_f64(1.0).unwrap(),
            remaining_quantity: Quantity::from_f64(1.0 - filled).unwrap(),
            filled_quantity: Quantity::from_f64(filled).unwrap(),
            price: Some(Price::from_f64(100.0).unwrap()),
            status: ports::OrderStatus::Acknowledged,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn test_reconcile_no_discrepancies() {
        let mut oms = OmsCore::new();
        let oid = OrderId("R-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // ACK the order
        let ack = ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&ack);

        // Exchange has same order with no fills
        let exchange_orders = vec![create_exchange_order("R-1", "BTCUSDT", 0.0)];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        assert!(!report.has_discrepancies());
        assert_eq!(report.total_discrepancies(), 0);
    }

    #[test]
    fn test_reconcile_exchange_only() {
        let oms = OmsCore::new();

        // Exchange has order that OMS doesn't know about
        let exchange_orders = vec![create_exchange_order("UNKNOWN-1", "BTCUSDT", 0.0)];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        assert!(report.has_discrepancies());
        assert_eq!(report.exchange_only.len(), 1);
        assert_eq!(report.exchange_only[0].0, "UNKNOWN-1");
    }

    #[test]
    fn test_reconcile_local_only() {
        let mut oms = OmsCore::new();
        let oid = OrderId("LOCAL-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // ACK the order locally
        let ack = ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&ack);

        // Exchange has no orders
        let exchange_orders: Vec<ports::OpenOrder> = vec![];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        assert!(report.has_discrepancies());
        assert_eq!(report.local_only.len(), 1);
        assert_eq!(report.local_only[0].order_id.0, "LOCAL-1");
    }

    #[test]
    fn test_reconcile_quantity_mismatch() {
        let mut oms = OmsCore::new();
        let oid = OrderId("QTY-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // OMS thinks 0.3 filled
        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.3).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Exchange says 0.5 filled
        let exchange_orders = vec![create_exchange_order("QTY-1", "BTCUSDT", 0.5)];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        assert!(report.has_discrepancies());
        assert_eq!(report.qty_mismatch.len(), 1);
        let mismatch = &report.qty_mismatch[0];
        assert_eq!(mismatch.order_id.0, "QTY-1");
        assert_eq!(mismatch.local_filled, Quantity::from_f64(0.3).unwrap());
        assert_eq!(mismatch.exchange_filled, Quantity::from_f64(0.5).unwrap());
    }

    #[test]
    fn test_reconcile_mixed_discrepancies() {
        let mut oms = OmsCore::new();

        // Order 1: exists in both with quantity mismatch
        let oid1 = OrderId("MIX-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid1.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let fill = ExecutionEvent::Fill {
            order_id: oid1.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.2).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Order 2: local only
        let oid2 = OrderId("MIX-2".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid2.clone(),
            client_order_id: None,
            symbol: Symbol::new("ETHUSDT"),
            side: Side::Sell,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let ack = ExecutionEvent::OrderAck {
            order_id: oid2.clone(),
            timestamp: 0,
        };
        let _ = oms.on_execution_event(&ack);

        // Exchange has MIX-1 with different fill + MIX-3 (unknown to OMS)
        let exchange_orders = vec![
            create_exchange_order("MIX-1", "BTCUSDT", 0.4), // qty mismatch
            create_exchange_order("MIX-3", "SOLUSDT", 0.0), // exchange only
        ];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        assert!(report.has_discrepancies());
        assert_eq!(report.total_discrepancies(), 3);
        assert_eq!(report.exchange_only.len(), 1); // MIX-3
        assert_eq!(report.local_only.len(), 1); // MIX-2
        assert_eq!(report.qty_mismatch.len(), 1); // MIX-1
    }

    #[test]
    fn test_reconcile_ignores_filled_orders() {
        let mut oms = OmsCore::new();
        let oid = OrderId("FILLED-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });

        // Fully fill the order locally
        let fill = ExecutionEvent::Fill {
            order_id: oid.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 0,
            fill_id: "f1".into(),
        };
        let _ = oms.on_execution_event(&fill);

        // Exchange has no orders (filled orders are removed)
        let exchange_orders: Vec<ports::OpenOrder> = vec![];
        let report = oms.reconcile_with_exchange(&exchange_orders);

        // Filled orders should not be reported as local-only
        assert!(!report.has_discrepancies());
    }
}
