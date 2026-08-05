//! 核心 traits - 適配器實現的穩定契約

use crate::events::*;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use hft_core::*;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;

/// 裝箱的事件流
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = HftResult<T>> + Send>>;

/// 連線健康狀態
#[derive(Debug, Clone)]
pub struct ConnectionHealth {
    pub connected: bool,
    pub latency_ms: Option<f64>,
    pub last_heartbeat: Timestamp,
}

/// 市場數據流接口 (公有行情)
#[async_trait]
pub trait MarketStream: Send + Sync {
    /// 訂閱指定品種，返回統一事件流
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>>;

    /// Latency-aware stream. Adapters with WS-library complete-message timing override this;
    /// the default is explicitly tagged as adapter publish and excluded from receive cohorts.
    async fn subscribe_tracked(
        &self,
        symbols: Vec<Symbol>,
    ) -> HftResult<BoxStream<TrackedMarketEvent>> {
        let stream = self.subscribe(symbols).await?;
        Ok(Box::pin(
            stream.map(|result| result.map(TrackedMarketEvent::new)),
        ))
    }

    /// 訂閱帶產品語義的品種。默認降級到 symbol-only，具體 adapter 可覆寫。
    async fn subscribe_instruments(
        &self,
        instruments: Vec<InstrumentSpec>,
    ) -> HftResult<BoxStream<MarketEvent>> {
        let symbols = instruments
            .into_iter()
            .map(|instrument| instrument.symbol)
            .collect();
        self.subscribe(symbols).await
    }

    async fn subscribe_tracked_instruments(
        &self,
        instruments: Vec<InstrumentSpec>,
    ) -> HftResult<BoxStream<TrackedMarketEvent>> {
        let symbols = instruments
            .into_iter()
            .map(|instrument| instrument.symbol)
            .collect();
        self.subscribe_tracked(symbols).await
    }

    /// 健康檢查
    async fn health(&self) -> ConnectionHealth;

    /// 開始連線
    async fn connect(&mut self) -> HftResult<()>;

    /// 斷開連線
    async fn disconnect(&mut self) -> HftResult<()>;
}

/// 執行客戶端接口 (私有流 + 下單)
#[async_trait]
pub trait ExecutionClient: Send + Sync {
    /// 下單 (live/mock 實現不同)
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId>;

    /// Idempotent live-order boundary. Adapters should forward `client_order_id` to the venue.
    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        self.place_order(envelope.intent.clone()).await
    }

    /// Placement with adapter-proven userspace boundaries. The default explicitly supplies no
    /// transport evidence instead of timing the outer async call and mislabeling it as a write.
    async fn place_order_envelope_traced(
        &mut self,
        envelope: &OrderIntentEnvelope,
    ) -> ExecutionSubmissionAttempt {
        ExecutionSubmissionAttempt::without_transport_timing(
            self.place_order_envelope(envelope).await,
        )
    }

    /// 帶 VenueSpec 校驗的下單
    async fn place_order_with_spec(
        &mut self,
        intent: OrderIntent,
        _venue_spec: Option<&VenueSpec>,
    ) -> HftResult<OrderId> {
        // 默認實現：調用普通下單方法 (向後兼容)
        self.place_order(intent).await
    }

    /// 撤單
    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()>;

    /// 修改訂單
    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()>;

    /// 執行回報流 (填充、ACK、拒絕等)
    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>>;

    /// Whether normal completion is part of this client's execution-stream contract.
    /// Live venue adapters must keep the default fail-closed behavior.
    fn execution_stream_may_complete(&self) -> bool {
        false
    }

    /// Returns true only for Monday's in-process simulated adapter. Simulated Paper/Shadow runs
    /// do not represent an external account and therefore do not enter real-account admission.
    fn is_simulated_execution(&self) -> bool {
        false
    }

    /// Confirms that the engine has applied a generation-tagged synchronization marker and every
    /// earlier execution report to OMS, portfolio, and risk state. Live adapters may keep their
    /// placement gate closed until this acknowledgement arrives.
    fn acknowledge_execution_stream_applied(&self, _stream_id: u64) {}

    /// 獲取未結訂單列表 (用於對賬)
    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>>;

    /// 獲取帳戶餘額 (用於餘額同步)
    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        Err(HftError::Config(
            "execution client does not support authoritative balance snapshots".to_string(),
        ))
    }

    /// Declares how this client represents account holdings. A Spot wallet is an asset inventory,
    /// not a derivatives position; unknown clients remain fail-closed.
    fn asset_inventory_capability(&self) -> AssetInventoryCapability {
        if self.supports_position_snapshot() {
            AssetInventoryCapability::PositionSnapshotRequired
        } else {
            AssetInventoryCapability::Unsupported
        }
    }

    /// Converts one authoritative wallet snapshot into the declared asset-level inventory.
    /// The worker passes the same snapshot used for balance reconciliation so a receipt never
    /// combines two wallet reads.
    fn asset_inventory_from_balances(
        &self,
        balances: &[AccountBalance],
    ) -> HftResult<Vec<AssetInventoryRecord>> {
        balances
            .iter()
            .cloned()
            .map(AssetInventoryRecord::try_from)
            .collect()
    }

    /// Whether this client can return an account-level position snapshot.
    fn supports_position_snapshot(&self) -> bool {
        false
    }

    /// Fetches current venue positions for account inspection and reconciliation.
    async fn get_positions(&self) -> HftResult<Vec<Position>> {
        Err(HftError::Config(
            "execution client does not support authoritative position snapshots".to_string(),
        ))
    }

    /// Whether this client can return a recent authoritative fill snapshot.
    fn supports_recent_fills_snapshot(&self) -> bool {
        false
    }

    /// Fetches recent account fills. Adapters must page until their venue reports completion.
    async fn list_recent_fills(&self) -> HftResult<Vec<AccountFill>> {
        Err(HftError::Config(
            "execution client does not support recent fill snapshots".to_string(),
        ))
    }

    /// 連線管理
    async fn connect(&mut self) -> HftResult<()>;
    async fn disconnect(&mut self) -> HftResult<()>;
    async fn health(&self) -> ConnectionHealth;
}

/// 帳戶餘額信息（從交易所同步）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalance {
    /// 資產名稱 (如 USDT, BTC)
    pub asset: String,
    /// 可用餘額
    pub available: rust_decimal::Decimal,
    /// 凍結餘額（掛單佔用）
    pub frozen: rust_decimal::Decimal,
    /// 總餘額
    pub total: rust_decimal::Decimal,
    /// 估值（以報價幣計價，如 USD）
    pub usd_value: Option<rust_decimal::Decimal>,
}

<<<<<<< HEAD
/// Typed asset-level inventory used for Spot reconciliation. This is deliberately not a
/// `Position`: the venue's asset identity and available/locked amounts remain intact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetInventoryRecord {
    pub asset: String,
    pub available: rust_decimal::Decimal,
    pub locked: rust_decimal::Decimal,
    pub total: rust_decimal::Decimal,
    pub usd_value: Option<rust_decimal::Decimal>,
}

impl AssetInventoryRecord {
    pub fn validate(&self) -> HftResult<()> {
        if self.asset.trim().is_empty() {
            return Err(HftError::Parse(
                "asset inventory has an empty asset".to_string(),
            ));
        }
        if self.available < rust_decimal::Decimal::ZERO
            || self.locked < rust_decimal::Decimal::ZERO
            || self.total < rust_decimal::Decimal::ZERO
        {
            return Err(HftError::Parse(format!(
                "asset inventory {} contains a negative amount",
                self.asset
            )));
        }
        if self.available + self.locked != self.total {
            return Err(HftError::Parse(format!(
                "asset inventory {} does not balance available + locked = total",
                self.asset
            )));
        }
        if self.total != rust_decimal::Decimal::ZERO
            && self
                .usd_value
                .is_none_or(|value| value < rust_decimal::Decimal::ZERO)
        {
            return Err(HftError::Parse(format!(
                "asset inventory {} is missing a non-negative valuation",
                self.asset
            )));
        }
        Ok(())
    }
}

impl TryFrom<AccountBalance> for AssetInventoryRecord {
    type Error = HftError;

    fn try_from(balance: AccountBalance) -> HftResult<Self> {
        let record = Self {
            asset: balance.asset,
            available: balance.available,
            locked: balance.frozen,
            total: balance.total,
            usd_value: balance.usd_value,
        };
        record.validate()?;
        Ok(record)
    }
}

/// Explicit account-holdings capability. Unsupported/unknown is never healthy by inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetInventoryCapability {
    PositionSnapshotRequired,
    AuthoritativeAssetInventory { product_type: ProductType },
    Unsupported,
}

/// The only executable environments. Paper is deliberately not an account admission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountExecutionEnvironment {
    Testnet,
    Live,
}

/// Authoritative account status as observed from the venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountReadbackState {
    Enabled,
    Restricted,
    Disabled,
}

/// Opaque, externally read-back account facts. Identifiers link an audit record without carrying
/// credential material or raw exchange responses through the execution path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountExternalReadback {
    pub state: AccountReadbackState,
    pub balances: Vec<AccountBalance>,
    pub capability: AccountCapability,
    pub regional_compliance_attestation_id: String,
    pub receipt_id: String,
    pub evidence_digest: String,
    pub validated_at: Timestamp,
    pub valid_until: Timestamp,
}

/// Runtime-owned admission record required before an intent can reach an execution adapter.
/// `credential_reference` is an opaque secret-manager reference; credential values never belong
/// in this contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountExecutionAdmission {
    pub account_id: AccountId,
    pub venue: VenueId,
    pub product_type: ProductType,
    pub environment: AccountExecutionEnvironment,
    pub credential_reference: String,
    pub readback: AccountExternalReadback,
    pub max_order_notional: rust_decimal::Decimal,
    pub max_open_orders: usize,
    pub kill_switch_active: bool,
    pub ready: bool,
}

/// Account-level fill record used by control-plane inspection and REST catch-up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountFill {
    pub fill_id: String,
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,
    pub quantity: Quantity,
    pub fee: Option<rust_decimal::Decimal>,
    pub timestamp: Timestamp,
}

/// 帳戶視圖 (策略決策用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountView {
    /// Account identity supplied by the runtime's account-snapshot publisher.
    #[serde(default)]
    pub account_id: Option<AccountId>,
    /// Runtime-owned bStocks admission evidence keyed by venue, product, and symbol.
    /// Strategy-supplied `OrderIntent` evidence is never an authorization source.
    #[serde(default)]
    pub tokenized_securities_attestations:
        std::collections::HashMap<InstrumentKey, TokenizedSecuritiesRuntimeAttestation>,
    /// Local asset-level inventory model. Wallet assets are never coerced into `positions`.
    #[serde(default)]
    pub asset_inventory: std::collections::HashMap<String, AssetInventoryRecord>,
    pub cash_balance: rust_decimal::Decimal,
    pub positions: std::collections::HashMap<Symbol, Position>,
    pub unrealized_pnl: rust_decimal::Decimal,
    pub realized_pnl: rust_decimal::Decimal,
    /// 高水位標記 (帳戶權益歷史最高值)
    pub high_water_mark: rust_decimal::Decimal,
    /// 當前回撤百分比 ((high_water_mark - equity) / high_water_mark * 100)
    pub drawdown_pct: f64,
    /// 歷史最大回撤百分比
    pub max_drawdown_pct: f64,
    /// 會話開始時間 (微秒時間戳)
    pub session_start_us: u64,
}

impl Default for AccountView {
    fn default() -> Self {
        Self {
            account_id: None,
            tokenized_securities_attestations: std::collections::HashMap::new(),
            asset_inventory: std::collections::HashMap::new(),
            cash_balance: rust_decimal::Decimal::ZERO,
            positions: std::collections::HashMap::new(),
            unrealized_pnl: rust_decimal::Decimal::ZERO,
            realized_pnl: rust_decimal::Decimal::ZERO,
            high_water_mark: rust_decimal::Decimal::ZERO,
            drawdown_pct: 0.0,
            max_drawdown_pct: 0.0,
            session_start_us: 0,
        }
    }
}

/// Runtime-owned bStocks admission evidence scoped to one account, product, and symbol.
///
/// The runtime account-snapshot publisher owns this record. It is intentionally separate from
/// `OrderIntent::compliance_context`, which strategies can populate and therefore cannot grant
/// trading authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenizedSecuritiesRuntimeAttestation {
    pub account_id: AccountId,
    pub venue: VenueId,
    pub product_type: ProductType,
    pub symbol: Symbol,
    pub source_id: String,
    pub observed_at: Timestamp,
    pub jurisdiction: Option<String>,
    pub account_eligible: bool,
    pub account_capability: AccountCapability,
    /// Gross notional for this symbol from the same reconciled account snapshot.
    pub account_symbol_notional: rust_decimal::Decimal,
    /// Gross tokenized-security notional from the same reconciled account snapshot.
    pub account_asset_class_notional: rust_decimal::Decimal,
    pub corporate_action_active: bool,
    pub top_depth_usd: rust_decimal::Decimal,
    pub spread_bps: rust_decimal::Decimal,
}

impl AccountView {
    /// 總 PnL (已實現 + 未實現)
    pub fn total_pnl(&self) -> rust_decimal::Decimal {
        self.realized_pnl + self.unrealized_pnl
    }

    /// 帳戶權益 = 現金 + 持倉成本價值 + 未實現盈虧。
    /// 已實現盈虧已反映在現金中，不應重覆相加。
    pub fn equity(&self) -> rust_decimal::Decimal {
        self.cash_balance
            + self
                .positions
                .values()
                .map(|position| {
                    position.avg_price.0 * position.quantity.0 + position.unrealized_pnl
                })
                .sum::<rust_decimal::Decimal>()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub quantity: Quantity,
    pub avg_price: Price,
    pub unrealized_pnl: rust_decimal::Decimal,
    /// Realized PnL accumulated while the current position lifecycle is open.
    #[serde(default)]
    pub realized_pnl: rust_decimal::Decimal,
}

/// 策略接口
/// 策略處理的市場範疇
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueScope {
    Single,
    Cross,
}

/// Read-only, sequence-validated L2 book exposed to strategies on the hot path.
///
/// The engine owns and rebuilds this view from snapshots and price-keyed deltas.
/// Strategies must not maintain a second interpretation of exchange delta semantics.
#[derive(Debug, Clone, Copy)]
pub struct L2BookView<'a> {
    pub symbol: &'a Symbol,
    pub venue: VenueId,
    pub timestamp: Timestamp,
    pub sequence: u64,
    pub bid_prices: &'a [FixedPrice],
    pub bid_quantities: &'a [FixedQuantity],
    pub ask_prices: &'a [FixedPrice],
    pub ask_quantities: &'a [FixedQuantity],
}

/// Stable strategy input. `book` is present when the event identifies a venue and symbol
/// whose canonical L2 state is currently synchronized.
#[derive(Debug, Clone, Copy)]
pub struct StrategyContext<'a> {
    pub account: &'a AccountView,
    pub book: Option<L2BookView<'a>>,
}

pub trait Strategy: Send + Sync {
    /// 處理市場事件，返回交易意圖
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent>;

    /// Process an event with the engine's latest canonical L2 state.
    ///
    /// Existing non-LOB strategies remain source-compatible through this default implementation.
    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        self.on_market_event(event, context.account)
    }

    /// 處理執行事件 (成交回報等)
    fn on_execution_event(
        &mut self,
        event: &ExecutionEvent,
        account: &AccountView,
    ) -> Vec<OrderIntent>;

    /// 策略名稱
    fn name(&self) -> &str;
    /// 策略實例ID（預設等同於 name；可被覆寫以回傳穩定實例ID）
    fn id(&self) -> &str {
        self.name()
    }
    /// 策略場域範疇（單場/跨場）；預設單場，可由策略覆寫
    fn venue_scope(&self) -> VenueScope {
        VenueScope::Single
    }

    /// 策略支援的資產類別。舊策略預設只支援 Crypto，避免誤吃證券型 token。
    fn supported_asset_classes(&self) -> &'static [AssetClass] {
        &[AssetClass::Crypto]
    }

    /// 策略初始化
    fn initialize(&mut self) -> HftResult<()> {
        Ok(())
    }

    /// 策略清理
    fn shutdown(&mut self) -> HftResult<()> {
        Ok(())
    }

    /// 向下转型支持（用于运行时类型检查）
    fn as_any(&self) -> &dyn std::any::Any {
        panic!("as_any not implemented for this strategy")
    }

    /// 向下转型支持（可变引用）
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        panic!("as_any_mut not implemented for this strategy")
    }
}

/// 風控決策
#[derive(Debug, Clone)]
pub enum RiskDecision {
    Allow,
    Reject {
        reason: String,
    },
    Modify {
        new_quantity: Quantity,
        reason: String,
    },
}

/// 交易所規格（作為穩定契約的一部分）
#[derive(Debug, Clone)]
pub struct VenueSpec {
    pub name: String,
    // 精度/步進
    pub tick_size: Price,
    pub lot_size: Quantity,
    // 數量/名義約束
    pub min_qty: Quantity,
    pub max_quantity: Option<Quantity>,
    pub min_notional: rust_decimal::Decimal,
    // 風險/費率/限流（可選）
    pub maker_fee_bps: Option<rust_decimal::Decimal>,
    pub taker_fee_bps: Option<rust_decimal::Decimal>,
    pub rate_limit: Option<u32>, // 每秒請求限制
}

impl Default for VenueSpec {
    fn default() -> Self {
        Self {
            name: "DEFAULT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.001).unwrap(),
            min_qty: Quantity::from_f64(0.001).unwrap(),
            max_quantity: None,
            min_notional: rust_decimal::Decimal::from(10),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }
}

impl VenueSpec {
    /// Phase 1 重構：為常見交易所創建預設 VenueSpec
    pub fn binance_spot() -> Self {
        Self {
            name: "BINANCE".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(), // 通用價格精度
            lot_size: Quantity::from_f64(0.00001).unwrap(), // 5位小數
            min_qty: Quantity::from_f64(0.00001).unwrap(),
            max_quantity: Some(Quantity::from_f64(900000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(10), // 10 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(1200),                        // 1200 requests/minute
        }
    }

    pub fn bitget_spot() -> Self {
        Self {
            name: "BITGET".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(), // 4位小數
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1000000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5), // 5 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(600),                        // 600 requests/minute
        }
    }

    pub fn bybit_spot() -> Self {
        Self {
            name: "BYBIT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.000001).unwrap(), // 6位小數
            min_qty: Quantity::from_f64(0.000001).unwrap(),
            max_quantity: Some(Quantity::from_f64(500000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(1), // 1 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(120),                        // 120 requests/minute
        }
    }

    pub fn okx_spot() -> Self {
        Self {
            name: "OKX".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.000001).unwrap(),
            min_qty: Quantity::from_f64(0.000001).unwrap(),
            max_quantity: Some(Quantity::from_f64(500000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: Some(rust_decimal::Decimal::new(8, 4)), // 0.08%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.10%
            rate_limit: Some(180),
        }
    }

    pub fn hyperliquid_spot() -> Self {
        // 占位默认规格，后续可依据官方文档细化
        Self {
            name: "HYPERLIQUID".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    pub fn backpack_spot() -> Self {
        // Backpack 官方最小步进依 symbol 不同，可透过市場 API 覆寫
        Self {
            name: "BACKPACK".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)),
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)),
            rate_limit: Some(600),
        }
    }

    pub fn lighter_spot() -> Self {
        // 參照公開資訊設置保守預設，後續可依據 lighter 規則更新
        Self {
            name: "LIGHTER".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    pub fn grvt_perp() -> Self {
        // GRVT 預設合約規格（暫以保守數值，實際可依 instrument API 覆寫）
        Self {
            name: "GRVT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    /// Polymarket tick size and minimum order size are market-specific and fetched by the live
    /// adapter immediately before signing. Zero precision/minimum values deliberately preserve
    /// the risk-reviewed intent here; the venue adapter remains the authoritative precision gate.
    pub fn polymarket_outcome() -> Self {
        Self {
            name: "POLYMARKET".to_string(),
            tick_size: Price(rust_decimal::Decimal::ZERO),
            lot_size: Quantity(rust_decimal::Decimal::ZERO),
            min_qty: Quantity(rust_decimal::Decimal::ZERO),
            max_quantity: None,
            min_notional: rust_decimal::Decimal::ZERO,
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    /// 構建預設的 VenueSpec 映射
    pub fn build_default_venue_specs() -> std::collections::HashMap<VenueId, VenueSpec> {
        let mut specs = std::collections::HashMap::new();
        specs.insert(VenueId::BINANCE, Self::binance_spot());
        specs.insert(VenueId::BITGET, Self::bitget_spot());
        specs.insert(VenueId::BYBIT, Self::bybit_spot());
        specs.insert(VenueId::OKX, Self::okx_spot());
        specs.insert(hft_core::VenueId::HYPERLIQUID, Self::hyperliquid_spot());
        specs.insert(hft_core::VenueId::BACKPACK, Self::backpack_spot());
        specs.insert(hft_core::VenueId::LIGHTER, Self::lighter_spot());
        specs.insert(hft_core::VenueId::GRVT, Self::grvt_perp());
        specs.insert(hft_core::VenueId::POLYMARKET, Self::polymarket_outcome());
        specs
    }
}

/// 風控管理器接口
pub trait RiskManager: Send + Sync {
    /// 審核訂單意圖，返回風控決策（舊版 - 字符串映射）
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent>;

    /// 審核訂單意圖（使用單個 VenueSpec）
    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue: &VenueSpec,
    ) -> Vec<OrderIntent>;

    /// Phase 1 重構：審核訂單意圖（使用 VenueId 映射）
    fn review_with_venue_specs(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<VenueId, VenueSpec>,
    ) -> Vec<OrderIntent> {
        // Keep a projected account across the whole batch. Calling review() with the same
        // account snapshot for every intent lets individually-valid orders exceed aggregate
        // position/notional limits when they are emitted in one engine tick.
        let mut approved_intents = Vec::new();
        let mut projected_account = account.clone();

        for intent in intents {
            if matches!(intent.asset_class, AssetClass::TokenizedSecurity)
                || matches!(intent.product_type, ProductType::TokenizedSecuritySpot)
            {
                let ctx = &intent.compliance_context;
                if !ctx.allow_tokenized_securities || !ctx.eligibility_confirmed {
                    eprintln!(
                        "Warning: tokenized security intent rejected before risk review: {:?}",
                        intent
                    );
                    continue;
                }
            }

            // 1. 優先使用 intent.target_venue
            let venue_spec = if let Some(target_venue) = intent.target_venue {
                venue_specs.get(&target_venue)
            } else {
                // 2. 回退到從 symbol 推斷 venue（簡單實現）
                // 這裡假設 symbol 格式為 "VENUE:SYMBOL" 或純 symbol
                let _base_symbol = BaseSymbol::from_venue_symbol(intent.symbol.as_str());
                // 簡化處理：使用第一個可用的 VenueSpec
                venue_specs.values().next()
            };

            if let Some(spec) = venue_spec {
                let reviewed = self.review(vec![intent], &projected_account, spec);
                for approved in &reviewed {
                    let signed_quantity = match approved.side {
                        Side::Buy => approved.quantity.0,
                        Side::Sell => -approved.quantity.0,
                    };
                    let position = projected_account
                        .positions
                        .entry(approved.symbol.clone())
                        .or_insert_with(|| Position {
                            symbol: approved.symbol.clone(),
                            quantity: Quantity::zero(),
                            avg_price: approved.price.unwrap_or_else(Price::zero),
                            unrealized_pnl: rust_decimal::Decimal::ZERO,
                            realized_pnl: rust_decimal::Decimal::ZERO,
                        });
                    position.quantity.0 += signed_quantity;
                    if let Some(price) = approved.price {
                        position.avg_price = price;
                    }
                }
                approved_intents.extend(reviewed);
            } else {
                // 沒有找到對應的 VenueSpec，拒絕此訂單
                eprintln!("Warning: No VenueSpec found for intent: {:?}", intent);
            }
        }

        approved_intents
    }

    /// Review intents without detaching their execution lifecycle metadata.
    fn review_envelopes_with_venue_specs(
        &mut self,
        envelopes: Vec<OrderIntentEnvelope>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<VenueId, VenueSpec>,
    ) -> Vec<OrderIntentEnvelope> {
        let mut approved_envelopes = Vec::new();
        let mut projected_account = account.clone();

        for envelope in envelopes {
            let OrderIntentEnvelope {
                intent,
                lifecycle,
                client_order_id,
                account_id,
            } = envelope;
            let reviewed =
                self.review_with_venue_specs(vec![intent], &projected_account, venue_specs);
            for approved in reviewed {
                let signed_quantity = match approved.side {
                    Side::Buy => approved.quantity.0,
                    Side::Sell => -approved.quantity.0,
                };
                let position = projected_account
                    .positions
                    .entry(approved.symbol.clone())
                    .or_insert_with(|| Position {
                        symbol: approved.symbol.clone(),
                        quantity: Quantity::zero(),
                        avg_price: approved.price.unwrap_or_else(Price::zero),
                        unrealized_pnl: rust_decimal::Decimal::ZERO,
                        realized_pnl: rust_decimal::Decimal::ZERO,
                    });
                position.quantity.0 += signed_quantity;
                if let Some(price) = approved.price {
                    position.avg_price = price;
                }
                approved_envelopes.push(OrderIntentEnvelope {
                    intent: approved,
                    lifecycle,
                    client_order_id: client_order_id.clone(),
                    account_id: account_id.clone(),
                });
            }
        }

        approved_envelopes
    }

    /// 處理執行事件
    fn on_execution_event(&mut self, event: &ExecutionEvent);

    /// 緊急停止
    fn emergency_stop(&mut self) -> Result<(), HftError>;

    /// 獲取風控指標
    fn get_risk_metrics(&self) -> std::collections::HashMap<String, rust_decimal::Decimal>;

    /// 是否應該暫停交易 (熔斷)
    fn should_halt_trading(&self, account: &AccountView) -> bool;

    /// 風控指標
    fn risk_metrics(&self) -> RiskMetrics;

    /// 動態更新風控配置
    ///
    /// 只更新提供的參數（Some 值），其他參數保持不變
    fn update_config(&mut self, update: RiskConfigUpdate) -> Result<(), HftError> {
        // 默認實現：不支持動態更新
        let _ = update;
        Err(HftError::Config(
            "此風控管理器不支持動態配置更新".to_string(),
        ))
    }

    /// 獲取當前風控配置（用於調試和狀態查詢）
    fn get_config_snapshot(&self) -> RiskConfigSnapshot {
        RiskConfigSnapshot::default()
    }
}

/// 風控配置更新請求
///
/// 所有字段都是可選的，只更新提供的值
#[derive(Debug, Clone, Default)]
pub struct RiskConfigUpdate {
    /// 最大回撤百分比 (0.0 - 100.0)
    pub max_drawdown_pct: Option<f64>,
    /// 最大持倉價值 (USD)
    pub max_position_usd: Option<f64>,
    /// 最大單筆訂單價值 (USD)
    pub max_order_size_usd: Option<f64>,
    /// 延遲閾值 (微秒)
    pub latency_threshold_us: Option<i64>,
    /// 最大訂單頻率 (每秒)
    pub max_orders_per_second: Option<i32>,
}

/// 風控配置快照（用於狀態查詢）
#[derive(Debug, Clone, Default)]
pub struct RiskConfigSnapshot {
    pub max_drawdown_pct: f64,
    pub max_position_usd: f64,
    pub max_order_size_usd: Option<f64>,
    pub latency_threshold_us: i64,
    pub max_orders_per_second: i32,
}

/// 風控指標
#[derive(Debug, Clone)]
pub struct RiskMetrics {
    pub max_drawdown: rust_decimal::Decimal,
    pub current_drawdown: rust_decimal::Decimal,
    pub var_1d: rust_decimal::Decimal, // 1日風險價值
    pub leverage: rust_decimal::Decimal,
    pub concentration_risk: rust_decimal::Decimal,
    pub order_rate: rust_decimal::Decimal, // 訂單頻率
    pub last_update: Timestamp,
}

// OrderStatus 已在 events 模組定義，此處直接使用 pub use crate::events::OrderStatus;

/// 訂單更新資訊
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub previous_status: OrderStatus,
}

/// 註冊訂單參數
#[derive(Debug, Clone)]
pub struct RegisterOrderParams {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub account_id: Option<hft_core::AccountId>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub venue: Option<hft_core::VenueId>,
    pub strategy_id: Option<String>,
}

/// 訂單記錄
#[derive(Debug, Clone)]
pub struct OrderRecord {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub account_id: Option<hft_core::AccountId>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub status: OrderStatus,
    pub venue: Option<hft_core::VenueId>,
    pub strategy_id: Option<String>,
}

/// Local OMS versus authoritative exchange open-order reconciliation.
#[derive(Debug, Clone, Default)]
pub struct OrderReconciliationReport {
    pub exchange_only: Vec<OrderId>,
    pub local_only: Vec<LocalOnlyOrder>,
    pub qty_mismatch: Vec<QuantityMismatch>,
}

impl OrderReconciliationReport {
    pub fn has_discrepancies(&self) -> bool {
        !self.exchange_only.is_empty()
            || !self.local_only.is_empty()
            || !self.qty_mismatch.is_empty()
    }

    pub fn total_discrepancies(&self) -> usize {
        self.exchange_only.len() + self.local_only.len() + self.qty_mismatch.len()
    }
}

#[derive(Debug, Clone)]
pub struct LocalOnlyOrder {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub status: OrderStatus,
}

#[derive(Debug, Clone)]
pub struct QuantityMismatch {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub exchange_filled: Quantity,
    pub local_filled: Quantity,
}

/// 訂單管理器 trait - 提供訂單生命週期管理能力
pub trait OrderManager: Send + Sync {
    /// 註冊新訂單
    fn register_order(&mut self, params: RegisterOrderParams);

    /// 處理執行事件，返回訂單狀態更新
    fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<OrderUpdate>;

    /// 導出 OMS 狀態（供恢復/持久化使用）
    fn export_state(&self) -> std::collections::HashMap<OrderId, OrderRecord>;

    /// 導入 OMS 狀態（供恢復/持久化使用）
    fn import_state(&mut self, state: std::collections::HashMap<OrderId, OrderRecord>);

    /// 取得指定策略的未結訂單
    fn open_order_pairs_by_strategy(&self, strategy_id: &str) -> Vec<(OrderId, Symbol)>;

    /// Compare OMS truth with an authoritative exchange snapshot.
    fn reconcile_with_exchange(&self, exchange_orders: &[OpenOrder]) -> OrderReconciliationReport;
}

/// Portfolio 狀態（供持久化使用）
#[derive(Debug, Clone)]
pub struct PortfolioState {
    pub account_view: AccountView,
    pub order_meta: std::collections::HashMap<OrderId, (Symbol, Side)>,
    pub market_prices: std::collections::HashMap<Symbol, Price>,
    /// 已處理的成交ID（去重），恢復後避免重覆累計
    pub processed_fill_ids: std::collections::HashMap<OrderId, std::collections::HashSet<String>>,
    /// Engine accounting replay horizon, ordered oldest to newest. Hash-based portfolio fill sets
    /// cannot reconstruct recency once the bounded engine deduper reaches capacity.
    pub recent_accounting_event_ids: Vec<(OrderId, String)>,
}

/// Portfolio 管理器 trait - 提供帳戶會計能力
pub trait PortfolioManager: Send + Sync {
    /// 註冊訂單元資訊（供 fill 時查找 symbol/side）
    fn register_order(&mut self, order_id: OrderId, symbol: Symbol, side: Side);

    /// 處理執行事件
    fn on_execution_event(&mut self, event: &ExecutionEvent);

    /// 獲取帳戶視圖讀取器
    fn reader(&self) -> Arc<dyn hft_snapshot::SnapshotReader<AccountView>>;

    /// 更新市場價格並重新計算未實現盈虧
    fn update_market_prices(&mut self, prices: &std::collections::HashMap<Symbol, Price>);

    /// 導出 Portfolio 狀態（供恢復/持久化使用）
    fn export_state(&self) -> PortfolioState;

    /// 導入 Portfolio 狀態（供恢復/持久化使用）
    fn import_state(&mut self, state: PortfolioState);
}

#[cfg(test)]
mod venue_spec_tests {
    use super::*;

    #[test]
    fn polymarket_uses_dynamic_venue_precision_without_skipping_risk_routing() {
        let specs = VenueSpec::build_default_venue_specs();
        let spec = specs
            .get(&VenueId::POLYMARKET)
            .expect("Polymarket must have a risk-routing VenueSpec");

        assert_eq!(spec.name, "POLYMARKET");
        assert_eq!(spec.tick_size.0, rust_decimal::Decimal::ZERO);
        assert_eq!(spec.lot_size.0, rust_decimal::Decimal::ZERO);
        assert_eq!(spec.min_notional, rust_decimal::Decimal::ZERO);
    }
}
