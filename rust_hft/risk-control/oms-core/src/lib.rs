//! OMS Core（純邏輯）
//! - 訂單狀態機（Ack/Partial/Fill/Cancel/Rejected/Expired/Replaced）
//! - 冪等與路由策略（不含任何網路）

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use hft_core::{AccountId, OrderId, Price, Quantity, Side, Symbol, Timestamp};
use ports::ExecutionEvent;
use tracing::{debug, info, warn};

/// A confirmed execution report that cannot be applied to the canonical OMS
/// without inventing or losing state.  These records are intentionally kept
/// separate from order transitions so callers can fail closed and reconcile
/// the venue event later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationException {
    pub order_id: Option<OrderId>,
    pub reason: String,
    pub event: Option<ExecutionEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Local state is not reconciled with the authoritative venue state.
    Unknown,
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
    #[serde(default)]
    pub account_id: Option<AccountId>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    #[serde(default)]
    pub limit_price: Option<Price>,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub status: OrderStatus,
    /// Optional venue where this order is routed to
    pub venue: Option<hft_core::VenueId>,
    /// Optional strategy id that created this order
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub venue_order_id: Option<String>,
    #[serde(default)]
    pub venue_order_history: Vec<String>,
    #[serde(default)]
    pub rejection_reason: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub state_changed_at: Option<Timestamp>,
    /// Processed fill ids for de-duplication
    #[serde(default)]
    pub processed_fill_ids: HashSet<String>,
}

/// Parameters for registering a new order
#[derive(Debug, Clone)]
pub struct RegisterOrderParams {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub account_id: Option<AccountId>,
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
            account_id: params.account_id,
            symbol: params.symbol,
            side: params.side,
            qty: params.qty,
            limit_price: None,
            cum_qty: Quantity::zero(),
            avg_price: None,
            status: OrderStatus::New,
            venue: params.venue,
            strategy_id: params.strategy_id,
            revision: 0,
            venue_order_id: None,
            venue_order_history: Vec::new(),
            rejection_reason: None,
            last_error: None,
            state_changed_at: None,
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
    notional_contracts: HashMap<OrderId, NotionalFillContract>,
    reconciliation_exceptions: Vec<ReconciliationException>,
}

/// Canonical OMS checkpoint.  The stable `OrderManager` trait predates
/// reconciliation evidence, so concrete persistence uses this envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmsState {
    pub orders: HashMap<OrderId, OrderRecord>,
    #[serde(default)]
    pub notional_contracts: HashMap<OrderId, NotionalFillContract>,
    #[serde(default)]
    pub reconciliation_exceptions: Vec<ReconciliationException>,
}

/// Explicit contract for a prediction-market BUY whose requested quantity is
/// sized by an original limit notional. It is opt-in; ordinary quantity
/// orders, sells, and reduce-only orders remain strict quantity contracts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotionalFillContract {
    pub requested_notional: rust_decimal::Decimal,
    pub limit_price: Price,
    pub filled_notional: rust_decimal::Decimal,
    pub reduce_only: bool,
    pub approved_quantity: Quantity,
    pub approved_limit_price: Price,
    pub approved_filled_quantity: Quantity,
    #[serde(default)]
    pub approved_filled_notional: rust_decimal::Decimal,
}

impl OmsCore {
    pub fn new() -> Self {
        Self::default()
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

    /// Events which were observed but deliberately left unapplied.  The
    /// caller owns the retry/reconciliation policy; reading this does not
    /// clear the evidence.
    pub fn reconciliation_exceptions(&self) -> &[ReconciliationException] {
        &self.reconciliation_exceptions
    }

    /// Enables the only canonical quantity exception: a non-reduce BUY may be
    /// filled with extra shares when venue price improvement keeps the exact
    /// original limit notional. Callers must opt in explicitly.
    pub fn register_notional_fill_contract(
        &mut self,
        order_id: &OrderId,
        limit_price: Price,
        reduce_only: bool,
    ) -> bool {
        let Some(order) = self.orders.get(order_id) else {
            self.record_exception(
                Some(order_id.clone()),
                "notional fill contract references an unknown order",
                None,
            );
            return false;
        };
        let order_qty = order.qty;
        let existing_price = order.limit_price;
        if order.side != Side::Buy || reduce_only || limit_price.0 <= rust_decimal::Decimal::ZERO {
            self.record_exception(
                Some(order_id.clone()),
                "notional fill contract is only valid for non-reduce BUY orders",
                None,
            );
            return false;
        }
        if let Some(existing_price) = existing_price {
            if existing_price != limit_price {
                self.record_exception(
                    Some(order_id.clone()),
                    "notional fill contract price disagrees with current order price",
                    None,
                );
                return false;
            }
        } else if let Some(order) = self.orders.get_mut(order_id) {
            order.limit_price = Some(limit_price);
        }
        self.notional_contracts.insert(
            order_id.clone(),
            NotionalFillContract {
                requested_notional: order_qty.0 * limit_price.0,
                limit_price,
                filled_notional: rust_decimal::Decimal::ZERO,
                reduce_only,
                approved_quantity: order_qty,
                approved_limit_price: limit_price,
                approved_filled_quantity: Quantity::zero(),
                approved_filled_notional: rust_decimal::Decimal::ZERO,
            },
        );
        true
    }

    /// 註冊新下單（由引擎在 place_order 成功前後調用）
    pub fn register_order(&mut self, params: RegisterOrderParams) -> bool {
        if let Some(existing) = self.orders.get(&params.order_id) {
            let same_payload = existing.client_order_id == params.client_order_id
                && existing.account_id == params.account_id
                && existing.symbol == params.symbol
                && existing.side == params.side
                && existing.qty == params.qty
                && existing.venue == params.venue
                && existing.strategy_id == params.strategy_id;
            if same_payload {
                // Registration is an idempotent command.  In particular, do
                // not replace terminal state or the fill deduplication set.
                return true;
            }

            let order_id = params.order_id.clone();
            warn!(order_id = %order_id.0, "refused conflicting canonical order registration");
            self.record_exception(Some(order_id), "conflicting order registration", None);
            return false;
        }

        let record = OrderRecord::new(params);
        if record.qty.0 <= rust_decimal::Decimal::ZERO {
            self.record_exception(
                Some(record.order_id.clone()),
                "order registration has non-positive quantity",
                None,
            );
            return false;
        }
        self.orders.insert(record.order_id.clone(), record);
        true
    }

    /// 應用執行事件（私有 WS 回報）更新狀態機
    /// 返回狀態更新，如果狀態變化為 Filled 則會在引擎層觸發 OrderCompleted 事件
    pub fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<OrderUpdate> {
        let referenced_order = match event {
            ExecutionEvent::OrderNew { .. }
            | ExecutionEvent::ConnectionStatus { .. }
            | ExecutionEvent::PrivateOrderTiming { .. }
            | ExecutionEvent::OrderLifecycleTiming { .. }
            | ExecutionEvent::ExecutionStreamBarrier { .. }
            | ExecutionEvent::ExecutionStreamSynchronized { .. }
            | ExecutionEvent::ReconciliationRequired { .. }
            | ExecutionEvent::BalanceUpdate { .. } => None,
            ExecutionEvent::OrderAck { order_id, .. }
            | ExecutionEvent::Fill { order_id, .. }
            | ExecutionEvent::FeeCharged { order_id, .. }
            | ExecutionEvent::OrderReject { order_id, .. }
            | ExecutionEvent::OrderCompleted { order_id, .. }
            | ExecutionEvent::OrderCanceled { order_id, .. }
            | ExecutionEvent::OrderModified { order_id, .. } => Some(order_id),
        };
        if let Some(order_id) = referenced_order.filter(|id| !self.orders.contains_key(*id)) {
            self.record_exception(
                Some(order_id.clone()),
                "execution event references an unknown order",
                Some(event),
            );
            return None;
        }
        match event {
            ExecutionEvent::OrderNew {
                order_id,
                client_order_id,
                account_id,
                symbol,
                side,
                quantity,
                requested_price,
                timestamp,
                venue,
                strategy_id,
                ..
            } => {
                self.register_order(RegisterOrderParams {
                    order_id: order_id.clone(),
                    client_order_id: client_order_id.clone(),
                    account_id: account_id.clone(),
                    symbol: symbol.clone(),
                    side: *side,
                    qty: *quantity,
                    venue: *venue,
                    strategy_id: Some(strategy_id.clone()).filter(|id| !id.is_empty()),
                });
                if let Some(order) = self.orders.get_mut(order_id) {
                    order.state_changed_at = Some(*timestamp);
                }
                if let Some(limit_price) = requested_price {
                    self.set_limit_price(order_id, *limit_price);
                }
            }
            ExecutionEvent::OrderAck {
                order_id,
                timestamp,
            } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    if matches!(ord.status, OrderStatus::New | OrderStatus::Unknown) {
                        ord.status = OrderStatus::Acknowledged;
                        ord.state_changed_at = Some(*timestamp);
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
            ExecutionEvent::Fill {
                order_id,
                price,
                quantity,
                fill_id,
                timestamp,
            } => {
                if fill_id.is_empty() {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill is missing the venue execution identity",
                        Some(event),
                    );
                    return None;
                }
                if quantity.0 <= rust_decimal::Decimal::ZERO {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill has non-positive quantity",
                        Some(event),
                    );
                    return None;
                }
                if price.0 <= rust_decimal::Decimal::ZERO {
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill has non-positive price",
                        Some(event),
                    );
                    return None;
                }
                let (already_processed, prev_cum_qty, order_qty) = {
                    let ord = self.orders.get(order_id).expect("checked above");
                    (
                        ord.processed_fill_ids.contains(fill_id),
                        ord.cum_qty.0,
                        ord.qty.0,
                    )
                };
                if already_processed {
                    debug!(
                        "Duplicate fill ignored: order_id={}, fill_id={}",
                        order_id.0, fill_id
                    );
                    return None;
                }
                let fill_qty = quantity.0;
                let new_cum_qty = prev_cum_qty + fill_qty;

                let notional_contract = self.notional_contracts.get(order_id);
                if let Some(contract) = notional_contract {
                    let next_notional = contract.filled_notional + fill_qty * price.0;
                    let valid_price = price.0 <= contract.limit_price.0;
                    let valid_quantity = new_cum_qty <= order_qty
                        || (!contract.reduce_only && price.0 < contract.limit_price.0);
                    if !valid_price
                        || !valid_quantity
                        || next_notional > contract.requested_notional
                    {
                        self.record_exception(
                            Some(order_id.clone()),
                            "fill exceeds the canonical order quantity/notional contract",
                            Some(event),
                        );
                        return None;
                    }
                } else if new_cum_qty > order_qty {
                    // A confirmed report that exceeds an ordinary quantity
                    // order is retained for reconciliation and never applied.
                    self.record_exception(
                        Some(order_id.clone()),
                        "fill exceeds the canonical order quantity contract",
                        Some(event),
                    );
                    return None;
                }

                if let Some(contract) = self.notional_contracts.get_mut(order_id) {
                    contract.filled_notional += fill_qty * price.0;
                }

                if let Some(ord) = self.orders.get_mut(order_id) {
                    // 精度保護：使用 Decimal 進行所有計算，避免浮點中間態
                    ord.processed_fill_ids.insert(fill_id.clone());

                    ord.cum_qty = Quantity(new_cum_qty);

                    // 狀態轉換：根據累計成交量判斷是否完全成交
                    let previous_status = ord.status;
                    let derived_status = if new_cum_qty >= ord.qty.0 {
                        OrderStatus::Filled
                    } else if previous_status == OrderStatus::Canceled {
                        // IOC/FAK cancellation can race its confirmed partial fill. Preserve the
                        // terminal remainder state while still accounting the late fill.
                        OrderStatus::Canceled
                    } else if previous_status == OrderStatus::Unknown {
                        // An unconfirmed late fill is retained as evidence while
                        // the order remains fail-closed until reconciliation.
                        OrderStatus::Unknown
                    } else {
                        OrderStatus::PartiallyFilled
                    };
                    // A late fill may be real after cancellation/rejection,
                    // but it must not resurrect the terminal order.
                    ord.status = if matches!(
                        previous_status,
                        OrderStatus::Canceled
                            | OrderStatus::Rejected
                            | OrderStatus::Expired
                            | OrderStatus::Replaced
                    ) {
                        previous_status
                    } else {
                        derived_status
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
                    ord.state_changed_at = Some(*timestamp);

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
            ExecutionEvent::OrderReject {
                order_id,
                reason,
                timestamp,
            } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    if !matches!(
                        ord.status,
                        OrderStatus::Filled
                            | OrderStatus::Canceled
                            | OrderStatus::Rejected
                            | OrderStatus::Expired
                            | OrderStatus::Replaced
                    ) {
                        ord.status = OrderStatus::Rejected;
                        ord.state_changed_at = Some(*timestamp);
                    }
                    ord.rejection_reason = Some(reason.clone());
                    return Some(OrderUpdate {
                        order_id: order_id.clone(),
                        status: ord.status,
                        cum_qty: ord.cum_qty,
                        avg_price: ord.avg_price,
                        previous_status,
                    });
                }
            }
            ExecutionEvent::OrderCanceled {
                order_id,
                timestamp,
            } => {
                if let Some(ord) = self.orders.get_mut(order_id) {
                    let previous_status = ord.status;
                    if !matches!(
                        ord.status,
                        OrderStatus::Filled
                            | OrderStatus::Canceled
                            | OrderStatus::Rejected
                            | OrderStatus::Expired
                            | OrderStatus::Replaced
                    ) {
                        ord.status = OrderStatus::Canceled;
                        ord.state_changed_at = Some(*timestamp);
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
            ExecutionEvent::OrderModified {
                order_id,
                new_quantity,
                new_price,
                timestamp,
            } => {
                return self.apply_order_modification(
                    order_id,
                    *new_quantity,
                    *new_price,
                    *timestamp,
                )
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

    pub fn apply_order_modification(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
        timestamp: Timestamp,
    ) -> Option<OrderUpdate> {
        let (previous_status, current_qty, cum_qty, current_price, side, contract) = {
            let order = self.orders.get(order_id)?;
            (
                order.status,
                order.qty,
                order.cum_qty,
                order.limit_price,
                order.side,
                self.notional_contracts.get(order_id).cloned(),
            )
        };
        let next_qty = new_quantity.unwrap_or(current_qty);
        if next_qty.0 <= rust_decimal::Decimal::ZERO || next_qty.0 < cum_qty.0 {
            self.record_exception(
                Some(order_id.clone()),
                "order modification quantity is below cumulative fill or non-positive",
                None,
            );
            return None;
        }
        let next_price = new_price.or(current_price);
        if new_price.is_some_and(|price| price.0 <= rust_decimal::Decimal::ZERO) {
            self.record_exception(
                Some(order_id.clone()),
                "order modification price is non-positive",
                None,
            );
            return None;
        }
        if let Some(contract) = contract.as_ref() {
            let Some(limit_price) = next_price else {
                self.record_exception(
                    Some(order_id.clone()),
                    "notional order modification removed its limit price",
                    None,
                );
                return None;
            };
            // `new_quantity` is the venue's total post-replace quantity.  A
            // confirmed partial fill is already spent and remains part of the
            // contract; only the remaining quantity is repriced.
            let remaining_qty = (next_qty.0 - cum_qty.0).max(rust_decimal::Decimal::ZERO);
            let requested_notional = contract.filled_notional + remaining_qty * limit_price.0;
            if side != Side::Buy
                || contract.reduce_only
                || contract.filled_notional > requested_notional
            {
                self.record_exception(
                    Some(order_id.clone()),
                    "order modification invalidates the canonical notional contract",
                    None,
                );
                return None;
            }
            if let Some(contract) = self.notional_contracts.get_mut(order_id) {
                contract.limit_price = limit_price;
                contract.requested_notional = requested_notional;
                contract.approved_quantity = next_qty;
                contract.approved_limit_price = limit_price;
                contract.approved_filled_quantity = cum_qty;
                contract.approved_filled_notional = contract.filled_notional;
            }
        }
        let order = self.orders.get_mut(order_id).expect("order checked above");
        order.qty = next_qty;
        if new_price.is_some() {
            order.limit_price = next_price;
        }
        order.revision = order.revision.saturating_add(1);
        if !matches!(
            previous_status,
            OrderStatus::Filled
                | OrderStatus::Canceled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Replaced
        ) {
            order.state_changed_at = Some(timestamp);
        }
        let derived_status = if order.cum_qty.0 >= order.qty.0 {
            OrderStatus::Filled
        } else if order.cum_qty.0 > rust_decimal::Decimal::ZERO {
            OrderStatus::PartiallyFilled
        } else {
            previous_status
        };
        if !matches!(
            previous_status,
            OrderStatus::Filled
                | OrderStatus::Canceled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Replaced
                | OrderStatus::Unknown
        ) {
            order.status = derived_status;
        }
        Some(OrderUpdate {
            order_id: order_id.clone(),
            status: order.status,
            cum_qty: order.cum_qty,
            avg_price: order.avg_price,
            previous_status,
        })
    }

    pub fn set_venue_order_id(&mut self, order_id: &OrderId, venue_order_id: String) -> bool {
        if venue_order_id.trim().is_empty() {
            self.record_exception(
                Some(order_id.clone()),
                "venue order identity must not be empty",
                None,
            );
            return false;
        }
        let Some(order) = self.orders.get_mut(order_id) else {
            self.record_exception(
                Some(order_id.clone()),
                "venue order identity references an unknown order",
                None,
            );
            return false;
        };
        if order.venue_order_id.as_deref() != Some(venue_order_id.as_str()) {
            if let Some(previous) = order.venue_order_id.replace(venue_order_id) {
                order.venue_order_history.push(previous);
            }
        }
        true
    }

    pub fn replace_venue_order_id(&mut self, order_id: &OrderId, venue_order_id: String) -> bool {
        if venue_order_id.trim().is_empty() {
            self.record_exception(
                Some(order_id.clone()),
                "venue order identity must not be empty",
                None,
            );
            return false;
        }
        let Some(order) = self.orders.get_mut(order_id) else {
            self.record_exception(
                Some(order_id.clone()),
                "venue order replacement references an unknown order",
                None,
            );
            return false;
        };
        if let Some(previous) = order.venue_order_id.replace(venue_order_id) {
            order.venue_order_history.push(previous);
        }
        true
    }

    pub fn set_limit_price(&mut self, order_id: &OrderId, price: Price) -> bool {
        if price.0 <= rust_decimal::Decimal::ZERO {
            self.record_exception(
                Some(order_id.clone()),
                "order limit price must be positive",
                None,
            );
            return false;
        }
        if self.notional_contracts.contains_key(order_id) {
            self.record_exception(
                Some(order_id.clone()),
                "notional order price must change through a canonical replace",
                None,
            );
            return false;
        }
        let Some(order) = self.orders.get_mut(order_id) else {
            self.record_exception(
                Some(order_id.clone()),
                "order limit price references an unknown order",
                None,
            );
            return false;
        };
        order.limit_price = Some(price);
        true
    }

    pub fn set_rejection_reason(&mut self, order_id: &OrderId, reason: String) -> bool {
        let Some(order) = self.orders.get_mut(order_id) else {
            return false;
        };
        order.rejection_reason = Some(reason);
        true
    }

    pub fn set_last_error(&mut self, order_id: &OrderId, error: String) -> bool {
        let Some(order) = self.orders.get_mut(order_id) else {
            return false;
        };
        order.last_error = Some(error);
        true
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
                    OrderStatus::Unknown
                        | OrderStatus::New
                        | OrderStatus::Acknowledged
                        | OrderStatus::PartiallyFilled
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
                    OrderStatus::Unknown
                        | OrderStatus::New
                        | OrderStatus::Acknowledged
                        | OrderStatus::PartiallyFilled
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
                OrderStatus::Unknown
                    | OrderStatus::New
                    | OrderStatus::Acknowledged
                    | OrderStatus::PartiallyFilled
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

    pub fn export_checkpoint(&self) -> OmsState {
        OmsState {
            orders: self.orders.clone(),
            notional_contracts: self.notional_contracts.clone(),
            reconciliation_exceptions: self.reconciliation_exceptions.clone(),
        }
    }

    /// Restore only a structurally self-consistent checkpoint.  A failed
    /// restore leaves the current state untouched and records the evidence.
    pub fn import_checkpoint(&mut self, state: OmsState) -> Result<(), String> {
        for (key, record) in &state.orders {
            if key != &record.order_id {
                let reason = format!(
                    "OMS checkpoint key does not match order record {:?}",
                    record.order_id
                );
                self.record_exception(None, reason.clone(), None);
                return Err(reason);
            }
            let legal_notional_overfill =
                state.notional_contracts.get(key).is_some_and(|contract| {
                    record.side == Side::Buy
                        && !contract.reduce_only
                        && contract.limit_price.0 > rust_decimal::Decimal::ZERO
                        && contract.filled_notional <= contract.requested_notional
                });
            if record.qty.0 <= rust_decimal::Decimal::ZERO
                || record.cum_qty.0 < rust_decimal::Decimal::ZERO
                || (record.cum_qty.0 > record.qty.0 && !legal_notional_overfill)
            {
                let reason = format!("OMS checkpoint has invalid quantities for {:?}", key);
                self.record_exception(Some(key.clone()), reason.clone(), None);
                return Err(reason);
            }
            if record
                .venue_order_id
                .as_ref()
                .is_some_and(|venue_order_id| venue_order_id.trim().is_empty())
                || record
                    .venue_order_history
                    .iter()
                    .any(|venue_order_id| venue_order_id.trim().is_empty())
            {
                let reason = format!("OMS checkpoint has invalid venue identity for {:?}", key);
                self.record_exception(Some(key.clone()), reason.clone(), None);
                return Err(reason);
            }
            if record
                .limit_price
                .is_some_and(|price| price.0 <= rust_decimal::Decimal::ZERO)
            {
                let reason = format!("OMS checkpoint has invalid limit price for {:?}", key);
                self.record_exception(Some(key.clone()), reason.clone(), None);
                return Err(reason);
            }
        }
        for (order_id, contract) in &state.notional_contracts {
            let Some(order) = state.orders.get(order_id) else {
                let reason = format!(
                    "OMS checkpoint notional contract references unknown order {:?}",
                    order_id
                );
                self.record_exception(Some(order_id.clone()), reason.clone(), None);
                return Err(reason);
            };
            let expected_notional = contract.approved_filled_notional
                + (contract.approved_quantity.0 - contract.approved_filled_quantity.0)
                    .max(rust_decimal::Decimal::ZERO)
                    * contract.approved_limit_price.0;
            if order.side != Side::Buy
                || contract.reduce_only
                || contract.requested_notional <= rust_decimal::Decimal::ZERO
                || contract.filled_notional < rust_decimal::Decimal::ZERO
                || contract.filled_notional > contract.requested_notional
                || contract.approved_quantity.0 <= rust_decimal::Decimal::ZERO
                || contract.approved_filled_quantity.0 < rust_decimal::Decimal::ZERO
                || contract.approved_filled_quantity.0 > contract.approved_quantity.0
                || contract.approved_filled_notional < rust_decimal::Decimal::ZERO
                || contract.approved_filled_notional > contract.requested_notional
                || contract.limit_price.0 <= rust_decimal::Decimal::ZERO
                || contract.approved_limit_price.0 <= rust_decimal::Decimal::ZERO
                || order.limit_price != Some(contract.limit_price)
                || contract.requested_notional != expected_notional
            {
                let reason = format!(
                    "OMS checkpoint has invalid notional contract for {:?}",
                    order_id
                );
                self.record_exception(Some(order_id.clone()), reason.clone(), None);
                return Err(reason);
            }
        }
        self.orders = state.orders;
        self.notional_contracts = state.notional_contracts;
        self.reconciliation_exceptions = state.reconciliation_exceptions;
        Ok(())
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
                    OrderStatus::Unknown
                        | OrderStatus::New
                        | OrderStatus::Acknowledged
                        | OrderStatus::PartiallyFilled
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
            ord.status = if previous_status == OrderStatus::Unknown {
                OrderStatus::Unknown
            } else if ord.cum_qty.0 >= ord.qty.0 {
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

        let mut matched_local_ids = HashSet::new();

        // Check for exchange-only and quantity mismatches
        for ex_order in exchange_orders {
            let local = self.orders.get_key_value(&ex_order.order_id).or_else(|| {
                ex_order
                    .client_order_id
                    .as_ref()
                    .and_then(|client_order_id| {
                        self.orders.iter().find(|(_, order)| {
                            order.client_order_id.as_ref() == Some(client_order_id)
                        })
                    })
            });
            if let Some((local_order_id, local_order)) = local {
                matched_local_ids.insert(local_order_id.clone());
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
                OrderStatus::Unknown
                    | OrderStatus::New
                    | OrderStatus::Acknowledged
                    | OrderStatus::PartiallyFilled
            ) && !matched_local_ids.contains(order_id)
            {
                report.local_only.push(LocalOnlyOrder {
                    order_id: order_id.clone(),
                    symbol: record.symbol.clone(),
                    status: match record.status {
                        OrderStatus::Unknown => ports::OrderStatus::Unknown,
                        OrderStatus::New => ports::OrderStatus::New,
                        OrderStatus::Acknowledged => ports::OrderStatus::Acknowledged,
                        OrderStatus::PartiallyFilled => ports::OrderStatus::PartiallyFilled,
                        OrderStatus::Filled => ports::OrderStatus::Filled,
                        OrderStatus::Canceled => ports::OrderStatus::Canceled,
                        OrderStatus::Rejected => ports::OrderStatus::Rejected,
                        OrderStatus::Expired => ports::OrderStatus::Expired,
                        OrderStatus::Replaced => ports::OrderStatus::Replaced,
                    },
                });
            }
        }

        report
    }
}

pub type ReconciliationReport = ports::OrderReconciliationReport;
pub type LocalOnlyOrder = ports::LocalOnlyOrder;
pub type QuantityMismatch = ports::QuantityMismatch;

// 實現 OrderManager trait
impl ports::OrderManager for OmsCore {
    fn register_order(&mut self, params: ports::RegisterOrderParams) -> bool {
        self.register_order(RegisterOrderParams {
            order_id: params.order_id,
            client_order_id: params.client_order_id,
            account_id: params.account_id,
            symbol: params.symbol,
            side: params.side,
            qty: params.qty,
            venue: params.venue,
            strategy_id: params.strategy_id,
        })
    }

    fn set_limit_price(&mut self, order_id: &OrderId, price: Price) -> bool {
        OmsCore::set_limit_price(self, order_id, price)
    }

    fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<ports::OrderUpdate> {
        self.on_execution_event(event)
            .map(|update| ports::OrderUpdate {
                order_id: update.order_id,
                status: match update.status {
                    OrderStatus::Unknown => ports::OrderStatus::Unknown,
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
                    OrderStatus::Unknown => ports::OrderStatus::Unknown,
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
                        account_id: rec.account_id.clone(),
                        symbol: rec.symbol.clone(),
                        side: rec.side,
                        qty: rec.qty,
                        limit_price: rec.limit_price,
                        cum_qty: rec.cum_qty,
                        avg_price: rec.avg_price,
                        status: match rec.status {
                            OrderStatus::Unknown => ports::OrderStatus::Unknown,
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
                        revision: rec.revision,
                        venue_order_id: rec.venue_order_id.clone(),
                        venue_order_history: rec.venue_order_history.clone(),
                        rejection_reason: rec.rejection_reason.clone(),
                        last_error: rec.last_error.clone(),
                        state_changed_at: rec.state_changed_at,
                        processed_fill_ids: rec.processed_fill_ids.clone(),
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
                        account_id: rec.account_id,
                        symbol: rec.symbol,
                        side: rec.side,
                        qty: rec.qty,
                        limit_price: rec.limit_price,
                        cum_qty: rec.cum_qty,
                        avg_price: rec.avg_price,
                        status: match rec.status {
                            ports::OrderStatus::Unknown => OrderStatus::Unknown,
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
                        revision: rec.revision,
                        venue_order_id: rec.venue_order_id,
                        venue_order_history: rec.venue_order_history,
                        rejection_reason: rec.rejection_reason,
                        last_error: rec.last_error,
                        state_changed_at: rec.state_changed_at,
                        processed_fill_ids: rec.processed_fill_ids,
                    },
                )
            })
            .collect();
        self.import_state(converted_state);
    }

    fn export_checkpoint(&self) -> ports::OmsCheckpoint {
        let checkpoint = OmsCore::export_checkpoint(self);
        ports::OmsCheckpoint {
            orders: checkpoint
                .orders
                .into_iter()
                .map(|(id, record)| {
                    (
                        id,
                        ports::OrderRecord {
                            order_id: record.order_id,
                            client_order_id: record.client_order_id,
                            account_id: record.account_id,
                            symbol: record.symbol,
                            side: record.side,
                            qty: record.qty,
                            limit_price: record.limit_price,
                            cum_qty: record.cum_qty,
                            avg_price: record.avg_price,
                            status: match record.status {
                                OrderStatus::Unknown => ports::OrderStatus::Unknown,
                                OrderStatus::New => ports::OrderStatus::New,
                                OrderStatus::Acknowledged => ports::OrderStatus::Acknowledged,
                                OrderStatus::PartiallyFilled => ports::OrderStatus::PartiallyFilled,
                                OrderStatus::Filled => ports::OrderStatus::Filled,
                                OrderStatus::Canceled => ports::OrderStatus::Canceled,
                                OrderStatus::Rejected => ports::OrderStatus::Rejected,
                                OrderStatus::Expired => ports::OrderStatus::Expired,
                                OrderStatus::Replaced => ports::OrderStatus::Replaced,
                            },
                            venue: record.venue,
                            strategy_id: record.strategy_id,
                            revision: record.revision,
                            venue_order_id: record.venue_order_id,
                            venue_order_history: record.venue_order_history,
                            rejection_reason: record.rejection_reason,
                            last_error: record.last_error,
                            state_changed_at: record.state_changed_at,
                            processed_fill_ids: record.processed_fill_ids,
                        },
                    )
                })
                .collect(),
            notional_contracts: checkpoint
                .notional_contracts
                .into_iter()
                .map(|(id, contract)| {
                    (
                        id,
                        ports::NotionalFillContract {
                            requested_notional: contract.requested_notional,
                            limit_price: contract.limit_price,
                            filled_notional: contract.filled_notional,
                            reduce_only: contract.reduce_only,
                            approved_quantity: contract.approved_quantity,
                            approved_limit_price: contract.approved_limit_price,
                            approved_filled_quantity: contract.approved_filled_quantity,
                            approved_filled_notional: contract.approved_filled_notional,
                        },
                    )
                })
                .collect(),
            reconciliation_exceptions: checkpoint
                .reconciliation_exceptions
                .into_iter()
                .map(|exception| ports::ReconciliationException {
                    order_id: exception.order_id,
                    reason: exception.reason,
                    event: exception.event,
                })
                .collect(),
        }
    }

    fn reconciliation_exception_count(&self) -> usize {
        self.reconciliation_exceptions.len()
    }

    fn import_checkpoint(&mut self, checkpoint: ports::OmsCheckpoint) -> Result<(), String> {
        let state = OmsState {
            orders: checkpoint
                .orders
                .into_iter()
                .map(|(id, record)| {
                    (
                        id,
                        OrderRecord {
                            order_id: record.order_id,
                            client_order_id: record.client_order_id,
                            account_id: record.account_id,
                            symbol: record.symbol,
                            side: record.side,
                            qty: record.qty,
                            limit_price: record.limit_price,
                            cum_qty: record.cum_qty,
                            avg_price: record.avg_price,
                            status: match record.status {
                                ports::OrderStatus::Unknown => OrderStatus::Unknown,
                                ports::OrderStatus::New => OrderStatus::New,
                                ports::OrderStatus::Acknowledged => OrderStatus::Acknowledged,
                                ports::OrderStatus::Accepted => OrderStatus::Acknowledged,
                                ports::OrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
                                ports::OrderStatus::Filled => OrderStatus::Filled,
                                ports::OrderStatus::Canceled => OrderStatus::Canceled,
                                ports::OrderStatus::Rejected => OrderStatus::Rejected,
                                ports::OrderStatus::Expired => OrderStatus::Expired,
                                ports::OrderStatus::Replaced => OrderStatus::Replaced,
                            },
                            venue: record.venue,
                            strategy_id: record.strategy_id,
                            revision: record.revision,
                            venue_order_id: record.venue_order_id,
                            venue_order_history: record.venue_order_history,
                            rejection_reason: record.rejection_reason,
                            last_error: record.last_error,
                            state_changed_at: record.state_changed_at,
                            processed_fill_ids: record.processed_fill_ids,
                        },
                    )
                })
                .collect(),
            notional_contracts: checkpoint
                .notional_contracts
                .into_iter()
                .map(|(id, contract)| {
                    (
                        id,
                        NotionalFillContract {
                            requested_notional: contract.requested_notional,
                            limit_price: contract.limit_price,
                            filled_notional: contract.filled_notional,
                            reduce_only: contract.reduce_only,
                            approved_quantity: contract.approved_quantity,
                            approved_limit_price: contract.approved_limit_price,
                            approved_filled_quantity: contract.approved_filled_quantity,
                            approved_filled_notional: contract.approved_filled_notional,
                        },
                    )
                })
                .collect(),
            reconciliation_exceptions: checkpoint
                .reconciliation_exceptions
                .into_iter()
                .map(|exception| ReconciliationException {
                    order_id: exception.order_id,
                    reason: exception.reason,
                    event: exception.event,
                })
                .collect(),
        };
        OmsCore::import_checkpoint(self, state)
    }

    fn open_order_pairs_by_strategy(&self, strategy_id: &str) -> Vec<(OrderId, Symbol)> {
        self.open_order_pairs_by_strategy(strategy_id)
    }

    fn reconcile_with_exchange(
        &self,
        exchange_orders: &[ports::OpenOrder],
    ) -> ports::OrderReconciliationReport {
        self.reconcile_with_exchange(exchange_orders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{AccountId, OrderId, Price, Quantity, Symbol, VenueId};

    #[test]
    fn test_ack_and_fill() {
        let mut oms = OmsCore::new();
        let oid = OrderId("T-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: Some(AccountId("bitget-testnet".to_string())),
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
        assert_eq!(
            order.account_id,
            Some(AccountId("bitget-testnet".to_string()))
        );
    }

    #[test]
    fn register_order_cannot_rebind_account_identity() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("ACCOUNT-BOUND".to_string());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: Some("client-1".to_string()),
            account_id: Some(AccountId("canonical-account".to_string())),
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).expect("valid quantity"),
            venue: Some(VenueId::BYBIT),
            strategy_id: Some("strategy-1".to_string()),
        });
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: Some("client-1".to_string()),
            account_id: Some(AccountId("wrong-account".to_string())),
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).expect("valid quantity"),
            venue: Some(VenueId::BYBIT),
            strategy_id: Some("strategy-1".to_string()),
        });

        assert_eq!(
            oms.get(&order_id)
                .and_then(|order| order.account_id.clone()),
            Some(AccountId("canonical-account".to_string()))
        );
    }

    #[test]
    fn test_order_rejected_removes_from_open() {
        let mut oms = OmsCore::new();
        let oid = OrderId("REJ-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            account_id: None,
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
            account_id: None,
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
    fn late_partial_fill_after_ioc_cancel_stays_terminal() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("IOC-LATE-FILL".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("123"),
            side: Side::Buy,
            qty: Quantity::from_f64(5.0).unwrap(),
            venue: Some(VenueId::POLYMARKET),
            strategy_id: Some("test".into()),
        });
        oms.on_execution_event(&ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: 1,
        });

        let update = oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.5).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: 2,
                fill_id: "late-confirmed".into(),
            })
            .expect("late fill is accounted");

        assert_eq!(update.status, OrderStatus::Canceled);
        assert_eq!(update.cum_qty, Quantity::from_f64(2.0).unwrap());
        assert!(oms.get_open_orders().is_empty());
    }

    #[test]
    fn test_weighted_avg_price_calculation() {
        let mut oms = OmsCore::new();
        let oid = OrderId("AVG-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            account_id: None,
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
            account_id: None,
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

    #[test]
    fn unfilled_order_modify_preserves_acknowledged_status() {
        let mut oms = OmsCore::new();
        let oid = OrderId("MOD-UNFILLED".into());
        oms.register_order(RegisterOrderParams {
            order_id: oid.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("123"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let _ = oms.on_execution_event(&ExecutionEvent::OrderAck {
            order_id: oid.clone(),
            timestamp: 0,
        });

        let update = oms
            .on_execution_event(&ExecutionEvent::OrderModified {
                order_id: oid,
                new_quantity: Some(Quantity::from_f64(8.0).unwrap()),
                new_price: Some(Price::from_f64(0.4).unwrap()),
                timestamp: 0,
            })
            .expect("order modified update");

        assert_eq!(update.status, OrderStatus::Acknowledged);
        assert_eq!(update.cum_qty, Quantity::zero());
    }

    #[test]
    fn identical_registration_preserves_terminal_state_and_fill_deduplication() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("IDEMPOTENT-REGISTER".into());
        let params = RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: Some("client".into()),
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: Some(VenueId::BYBIT),
            strategy_id: Some("strategy".into()),
        };
        assert!(oms.register_order(params.clone()));
        let fill = ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 1,
            fill_id: "venue-fill".into(),
        };
        assert_eq!(
            oms.on_execution_event(&fill).unwrap().status,
            OrderStatus::Filled
        );

        assert!(oms.register_order(params));
        assert!(oms.on_execution_event(&fill).is_none());
        assert_eq!(oms.get(&order_id).unwrap().status, OrderStatus::Filled);
        assert_eq!(
            oms.get(&order_id).unwrap().cum_qty,
            Quantity::from_f64(1.0).unwrap()
        );
    }

    #[test]
    fn terminal_status_cannot_be_regressed_by_late_ack_reject_or_cancel() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("TERMINAL-MONOTONIC".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let fill = ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: 1,
            fill_id: "terminal-fill".into(),
        };
        oms.on_execution_event(&fill).unwrap();
        let terminal_changed_at = oms.get(&order_id).unwrap().state_changed_at;
        for event in [
            ExecutionEvent::OrderAck {
                order_id: order_id.clone(),
                timestamp: 2,
            },
            ExecutionEvent::OrderReject {
                order_id: order_id.clone(),
                reason: "late".into(),
                timestamp: 3,
            },
            ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: 4,
            },
        ] {
            assert_eq!(
                oms.on_execution_event(&event).unwrap().status,
                OrderStatus::Filled
            );
            assert_eq!(
                oms.get(&order_id).unwrap().state_changed_at,
                terminal_changed_at
            );
        }
        assert_eq!(oms.get(&order_id).unwrap().status, OrderStatus::Filled);
    }

    #[test]
    fn terminal_order_modified_preserves_terminal_state_clock() {
        for (label, terminal_event, expected_status) in [
            (
                "filled",
                ExecutionEvent::Fill {
                    order_id: OrderId("TERMINAL-MODIFIED-FILLED".into()),
                    price: Price::from_f64(100.0).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap(),
                    timestamp: 10,
                    fill_id: "terminal-modified-fill".into(),
                },
                OrderStatus::Filled,
            ),
            (
                "canceled",
                ExecutionEvent::OrderCanceled {
                    order_id: OrderId("TERMINAL-MODIFIED-CANCELED".into()),
                    timestamp: 10,
                },
                OrderStatus::Canceled,
            ),
            (
                "rejected",
                ExecutionEvent::OrderReject {
                    order_id: OrderId("TERMINAL-MODIFIED-REJECTED".into()),
                    reason: "venue rejected".into(),
                    timestamp: 10,
                },
                OrderStatus::Rejected,
            ),
        ] {
            let mut oms = OmsCore::new();
            let order_id = match &terminal_event {
                ExecutionEvent::Fill { order_id, .. }
                | ExecutionEvent::OrderCanceled { order_id, .. }
                | ExecutionEvent::OrderReject { order_id, .. } => order_id.clone(),
                _ => unreachable!("test only covers terminal events"),
            };
            assert!(oms.register_order(RegisterOrderParams {
                order_id: order_id.clone(),
                client_order_id: None,
                account_id: None,
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                qty: Quantity::from_f64(1.0).unwrap(),
                venue: None,
                strategy_id: None,
            }));
            assert_eq!(
                oms.on_execution_event(&terminal_event).unwrap().status,
                expected_status
            );
            let terminal_changed_at = oms.get(&order_id).unwrap().state_changed_at;
            assert_eq!(terminal_changed_at, Some(10), "{label} terminal timestamp");

            let modified = oms
                .on_execution_event(&ExecutionEvent::OrderModified {
                    order_id: order_id.clone(),
                    new_quantity: Some(Quantity::from_f64(1.0).unwrap()),
                    new_price: Some(Price::from_f64(99.0).unwrap()),
                    timestamp: 20,
                })
                .expect("terminal modification remains observable");
            assert_eq!(modified.status, expected_status, "{label} status");
            assert_eq!(
                oms.get(&order_id).unwrap().state_changed_at,
                terminal_changed_at,
                "{label} late modification must not extend retention"
            );
        }
    }

    #[test]
    fn unknown_order_converges_on_ack_reject_or_cancel_without_reopening_terminal_state() {
        for (suffix, terminal_event, expected) in [
            (
                "ACK",
                ExecutionEvent::OrderAck {
                    order_id: OrderId("UNKNOWN-ACK".into()),
                    timestamp: 2,
                },
                OrderStatus::Acknowledged,
            ),
            (
                "REJECT",
                ExecutionEvent::OrderReject {
                    order_id: OrderId("UNKNOWN-REJECT".into()),
                    reason: "rejected".into(),
                    timestamp: 2,
                },
                OrderStatus::Rejected,
            ),
            (
                "CANCEL",
                ExecutionEvent::OrderCanceled {
                    order_id: OrderId("UNKNOWN-CANCEL".into()),
                    timestamp: 2,
                },
                OrderStatus::Canceled,
            ),
        ] {
            let mut oms = OmsCore::new();
            let order_id = OrderId(format!("UNKNOWN-{suffix}"));
            oms.register_order(RegisterOrderParams {
                order_id: order_id.clone(),
                client_order_id: None,
                account_id: None,
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                qty: Quantity::from_f64(1.0).unwrap(),
                venue: None,
                strategy_id: None,
            });
            oms.update_status(&order_id, OrderStatus::Unknown);
            let event = match terminal_event {
                ExecutionEvent::OrderAck { timestamp, .. } => ExecutionEvent::OrderAck {
                    order_id: order_id.clone(),
                    timestamp,
                },
                ExecutionEvent::OrderReject {
                    reason, timestamp, ..
                } => ExecutionEvent::OrderReject {
                    order_id: order_id.clone(),
                    reason,
                    timestamp,
                },
                ExecutionEvent::OrderCanceled { timestamp, .. } => ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp,
                },
                _ => unreachable!(),
            };
            assert_eq!(oms.on_execution_event(&event).unwrap().status, expected);
            assert_eq!(oms.get(&order_id).unwrap().status, expected);
        }
    }

    #[test]
    fn malformed_or_overfill_events_are_preserved_for_reconciliation() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("RECON-EVENT".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        for fill_id in ["", "too-large"] {
            assert!(oms
                .on_execution_event(&ExecutionEvent::Fill {
                    order_id: order_id.clone(),
                    price: Price::from_f64(100.0).unwrap(),
                    quantity: Quantity::from_f64(if fill_id.is_empty() { 0.1 } else { 2.0 })
                        .unwrap(),
                    timestamp: 1,
                    fill_id: fill_id.into(),
                })
                .is_none());
        }
        assert_eq!(oms.get(&order_id).unwrap().cum_qty, Quantity::zero());
        assert_eq!(oms.reconciliation_exceptions().len(), 2);
    }

    #[test]
    fn unknown_reconciliation_state_is_fail_closed_and_keeps_late_fill_accounting() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("UNKNOWN-STATE".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(2.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        oms.update_status(&order_id, OrderStatus::Unknown);
        let update = oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: 1,
                fill_id: "unknown-fill".into(),
            })
            .unwrap();
        assert_eq!(update.status, OrderStatus::Unknown);
        assert_eq!(update.cum_qty, Quantity::from_f64(1.0).unwrap());
        assert!(oms
            .get_open_orders()
            .iter()
            .any(|order| order.order_id == order_id));
    }

    #[test]
    fn checkpoint_restore_preserves_fill_deduplication_and_rejects_bad_references() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("CHECKPOINT".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let fill = ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.25).unwrap(),
            timestamp: 1,
            fill_id: "checkpoint-fill".into(),
        };
        oms.on_execution_event(&fill);
        let checkpoint = oms.export_checkpoint();

        let mut restored = OmsCore::new();
        restored.import_checkpoint(checkpoint).unwrap();
        assert!(restored.on_execution_event(&fill).is_none());
        assert_eq!(
            restored.get(&order_id).unwrap().cum_qty,
            Quantity::from_f64(0.25).unwrap()
        );

        let mut bad = restored.export_checkpoint();
        let record = bad.orders.remove(&order_id).unwrap();
        bad.orders.insert(OrderId("wrong-key".into()), record);
        assert!(restored.import_checkpoint(bad).is_err());
        assert_eq!(
            restored.get(&order_id).unwrap().cum_qty,
            Quantity::from_f64(0.25).unwrap()
        );
    }

    #[test]
    fn notional_contract_allows_only_exact_price_improved_buy_capacity() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("NOTIONAL-BUY".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN-UP"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(oms.register_notional_fill_contract(
            &order_id,
            Price::from_f64(0.50).unwrap(),
            false,
        ));
        let improved = ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price::from_f64(0.40).unwrap(),
            quantity: Quantity::from_f64(12.0).unwrap(),
            timestamp: 1,
            fill_id: "improved".into(),
        };
        assert_eq!(
            oms.on_execution_event(&improved).unwrap().status,
            OrderStatus::Filled
        );
        assert_eq!(
            oms.get(&order_id).unwrap().cum_qty,
            Quantity::from_f64(12.0).unwrap()
        );
        let checkpoint = oms.export_checkpoint();
        let mut restored = OmsCore::new();
        restored.import_checkpoint(checkpoint).unwrap();
        assert_eq!(
            restored.get(&order_id).unwrap().cum_qty,
            Quantity::from_f64(12.0).unwrap()
        );

        let mut strict = OmsCore::new();
        let strict_id = OrderId("STRICT-BUY".into());
        strict.register_order(RegisterOrderParams {
            order_id: strict_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN-UP"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(strict
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: strict_id.clone(),
                price: Price::from_f64(0.40).unwrap(),
                quantity: Quantity::from_f64(12.0).unwrap(),
                timestamp: 1,
                fill_id: "strict-overfill".into(),
            })
            .is_none());
        assert_eq!(strict.get(&strict_id).unwrap().cum_qty, Quantity::zero());
    }

    #[test]
    fn notional_contract_rejects_high_price_fills_even_within_quantity() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("NOTIONAL-HIGH-PRICE".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN-UP"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        oms.register_notional_fill_contract(&order_id, Price::from_f64(0.50).unwrap(), false);
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.60).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: 1,
                fill_id: "high-price".into(),
            })
            .is_none());
        assert_eq!(oms.get(&order_id).unwrap().cum_qty, Quantity::zero());
        assert_eq!(oms.reconciliation_exceptions().len(), 1);
    }

    fn create_exchange_order(id: &str, symbol: &str, filled: f64) -> ports::OpenOrder {
        ports::OpenOrder {
            order_id: OrderId(id.into()),
            client_order_id: None,
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
            account_id: None,
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
    fn reconciliation_matches_exchange_order_by_client_id() {
        let mut oms = OmsCore::new();
        let local_id = OrderId("provisional-client-1".into());
        oms.register_order(RegisterOrderParams {
            order_id: local_id.clone(),
            client_order_id: Some("client-1".into()),
            account_id: None,
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            qty: Quantity::from_f64(1.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        let _ = oms.on_execution_event(&ExecutionEvent::OrderAck {
            order_id: local_id,
            timestamp: 0,
        });
        let mut exchange_order = create_exchange_order("123", "BTCUSDT", 0.0);
        exchange_order.client_order_id = Some("client-1".into());

        let report = oms.reconcile_with_exchange(&[exchange_order]);

        assert!(!report.has_discrepancies());
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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
            account_id: None,
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

    #[test]
    fn order_modify_updates_price_and_notional_contract_before_fill() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("REPLACE-PRICE".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(oms.set_limit_price(&order_id, Price::from_f64(0.5).unwrap()));
        assert!(oms.register_notional_fill_contract(
            &order_id,
            Price::from_f64(0.5).unwrap(),
            false
        ));

        let update = oms
            .on_execution_event(&ExecutionEvent::OrderModified {
                order_id: order_id.clone(),
                new_quantity: Some(Quantity::from_f64(20.0).unwrap()),
                new_price: Some(Price::from_f64(0.4).unwrap()),
                timestamp: 7,
            })
            .expect("valid replace");
        assert_eq!(update.status, OrderStatus::New);
        assert_eq!(
            oms.get(&order_id).unwrap().qty,
            Quantity::from_f64(20.0).unwrap()
        );
        assert_eq!(
            oms.get(&order_id).unwrap().limit_price,
            Some(Price::from_f64(0.4).unwrap())
        );
        assert_eq!(oms.get(&order_id).unwrap().revision, 1);
        assert_eq!(
            oms.export_checkpoint()
                .notional_contracts
                .get(&order_id)
                .unwrap()
                .requested_notional,
            rust_decimal::Decimal::from(8)
        );
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.4).unwrap(),
                quantity: Quantity::from_f64(20.0).unwrap(),
                timestamp: 8,
                fill_id: "replace-fill".into(),
            })
            .is_some());
        assert!(oms.reconciliation_exceptions().is_empty());
    }

    #[test]
    fn partial_fill_replace_preserves_spent_notional_and_restores() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("REPLACE-PARTIAL".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(oms.set_limit_price(&order_id, Price::from_f64(0.5).unwrap()));
        assert!(oms.register_notional_fill_contract(
            &order_id,
            Price::from_f64(0.5).unwrap(),
            false
        ));
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.5).unwrap(),
                quantity: Quantity::from_f64(5.0).unwrap(),
                timestamp: 1,
                fill_id: "partial-1".into(),
            })
            .is_some());
        let pre_replace_checkpoint = oms.export_checkpoint();
        let mut pre_replace_restored = OmsCore::new();
        pre_replace_restored
            .import_checkpoint(pre_replace_checkpoint)
            .expect("partial fill before replace remains restorable");
        oms.on_execution_event(&ExecutionEvent::OrderModified {
            order_id: order_id.clone(),
            new_quantity: Some(Quantity::from_f64(7.0).unwrap()),
            new_price: Some(Price::from_f64(0.4).unwrap()),
            timestamp: 2,
        })
        .expect("partial replace");
        let contract = oms.export_checkpoint().notional_contracts[&order_id].clone();
        assert_eq!(contract.filled_notional, rust_decimal::Decimal::new(25, 1));
        assert_eq!(
            contract.requested_notional,
            rust_decimal::Decimal::new(33, 1)
        );
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.4).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: 3,
                fill_id: "partial-2".into(),
            })
            .is_some());
        let checkpoint = oms.export_checkpoint();
        let mut restored = OmsCore::new();
        restored
            .import_checkpoint(checkpoint)
            .expect("valid partial replacement checkpoint");
        assert_eq!(
            restored.get(&order_id).unwrap().limit_price,
            Some(Price::from_f64(0.4).unwrap())
        );
        assert_eq!(restored.get(&order_id).unwrap().revision, 1);
    }

    #[test]
    fn checkpoint_rejects_notional_cap_that_disagrees_with_current_order() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("CHECKPOINT-CAP".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(oms.set_limit_price(&order_id, Price::from_f64(0.5).unwrap()));
        assert!(oms.register_notional_fill_contract(
            &order_id,
            Price::from_f64(0.5).unwrap(),
            false
        ));
        let mut checkpoint = oms.export_checkpoint();
        checkpoint
            .notional_contracts
            .get_mut(&order_id)
            .unwrap()
            .requested_notional = rust_decimal::Decimal::from(100);
        let mut restored = OmsCore::new();
        assert!(restored.import_checkpoint(checkpoint).is_err());
    }

    #[test]
    fn improved_partial_fill_before_replace_keeps_fixed_budget_restorable() {
        let mut oms = OmsCore::new();
        let order_id = OrderId("REPLACE-IMPROVED-PARTIAL".into());
        oms.register_order(RegisterOrderParams {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new("TOKEN"),
            side: Side::Buy,
            qty: Quantity::from_f64(10.0).unwrap(),
            venue: None,
            strategy_id: None,
        });
        assert!(oms.set_limit_price(&order_id, Price::from_f64(0.5).unwrap()));
        assert!(oms.register_notional_fill_contract(
            &order_id,
            Price::from_f64(0.5).unwrap(),
            false
        ));
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.4).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: 1,
                fill_id: "improved-before-replace".into(),
            })
            .is_some());
        let mut restored_before_replace = OmsCore::new();
        restored_before_replace
            .import_checkpoint(oms.export_checkpoint())
            .expect("improved partial fill remains restorable");
        oms.on_execution_event(&ExecutionEvent::OrderModified {
            order_id: order_id.clone(),
            new_quantity: Some(Quantity::from_f64(7.0).unwrap()),
            new_price: Some(Price::from_f64(0.4).unwrap()),
            timestamp: 2,
        })
        .expect("replace after improved partial fill");
        let contract = oms.export_checkpoint().notional_contracts[&order_id].clone();
        assert_eq!(
            contract.requested_notional,
            rust_decimal::Decimal::new(28, 1)
        );
        assert!(oms
            .on_execution_event(&ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price::from_f64(0.4).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: 3,
                fill_id: "improved-after-replace".into(),
            })
            .is_some());
        let mut restored_after_replace = OmsCore::new();
        restored_after_replace
            .import_checkpoint(oms.export_checkpoint())
            .expect("improved partial replace checkpoint remains restorable");
    }
}
