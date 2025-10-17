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

impl OrderRecord {
    fn new(
        order_id: OrderId,
        client_order_id: Option<String>,
        symbol: Symbol,
        side: Side,
        qty: Quantity,
        venue: Option<hft_core::VenueId>,
        strategy_id: Option<String>,
    ) -> Self {
        Self {
            order_id,
            client_order_id,
            symbol,
            side,
            qty,
            cum_qty: Quantity::zero(),
            avg_price: None,
            status: OrderStatus::New,
            venue,
            strategy_id,
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
pub struct OmsCore {
    orders: HashMap<OrderId, OrderRecord>,
}

impl OmsCore {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
        }
    }

    /// 註冊新下單（由引擎在 place_order 成功前後調用）
    pub fn register_order(
        &mut self,
        order_id: OrderId,
        client_order_id: Option<String>,
        symbol: Symbol,
        side: Side,
        qty: Quantity,
        venue: Option<hft_core::VenueId>,
        strategy_id: Option<String>,
    ) {
        self.orders.insert(
            order_id.clone(),
            OrderRecord::new(
                order_id,
                client_order_id,
                symbol,
                side,
                qty,
                venue,
                strategy_id,
            ),
        );
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderId, Price, Quantity, Symbol};

    #[test]
    fn test_ack_and_fill() {
        let mut oms = OmsCore::new();
        let oid = OrderId("T-1".into());
        oms.register_order(
            oid.clone(),
            None,
            Symbol::new("BTCUSDT"),
            Side::Buy,
            Quantity::from_f64(1.0).unwrap(),
            None,
            Some("test_strategy".to_string()),
        );

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
        oms.register_order(
            o1.clone(),
            None,
            Symbol::new("BTCUSDT"),
            Side::Buy,
            Quantity::from_f64(1.0).unwrap(),
            None,
            Some("stratA".into()),
        );
        // o2: Ack (open)
        let o2 = OrderId("S-2".into());
        oms.register_order(
            o2.clone(),
            None,
            Symbol::new("ETHUSDT"),
            Side::Buy,
            Quantity::from_f64(2.0).unwrap(),
            None,
            Some("stratA".into()),
        );
        let _ = oms.on_execution_event(&ExecutionEvent::OrderAck {
            order_id: o2.clone(),
            timestamp: 0,
        });
        // o3: Filled (not open)
        let o3 = OrderId("S-3".into());
        oms.register_order(
            o3.clone(),
            None,
            Symbol::new("SOLUSDT"),
            Side::Buy,
            Quantity::from_f64(1.0).unwrap(),
            None,
            Some("stratB".into()),
        );
        let _ = oms.on_execution_event(&ExecutionEvent::Fill {
            order_id: o3,
            price: Price::from_f64(10.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 0,
            fill_id: "x".into(),
        });

        let counts = oms.open_counts_by_strategy();
        assert_eq!(counts.get("stratA"), Some(&2));
        assert!(counts.get("stratB").is_none());
    }
}
