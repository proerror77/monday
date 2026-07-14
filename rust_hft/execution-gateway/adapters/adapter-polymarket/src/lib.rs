//! Monday-native Polymarket CLOB execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as _;
use async_trait::async_trait;
use futures::{stream, Stream, StreamExt};
use hft_core::{
    now_micros, AssetClass, HftError, HftResult, OrderId, OrderType as HftOrderType, Price,
    ProductType, Quantity, Side, Symbol, TimeInForce, Timestamp, VenueId,
};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::types::request::{
    BalanceAllowanceRequest, MidpointRequest, OrdersRequest, TradesRequest,
};
use polymarket_client_sdk::clob::types::response::{
    CancelOrdersResponse, OpenOrderResponse, TradeResponse,
};
use polymarket_client_sdk::clob::types::{
    Amount, AssetType, OrderStatusType, OrderType as PolymarketOrderType, Side as PolymarketSide,
    SignatureType, TradeStatusType, TraderSide,
};
use polymarket_client_sdk::clob::ws::types::response::{
    OrderMessage, OrderMessageType, TradeMessage, TradeMessageStatus, WsMessage,
};
use polymarket_client_sdk::clob::ws::{ChannelType, Client as WsClient};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::types::response::Position as DataPosition;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::error::{Kind as SdkErrorKind, Status as SdkStatus};
use polymarket_client_sdk::types::{Address, B256, U256};
use polymarket_client_sdk::ws::config::Config as WsConfig;
use polymarket_client_sdk::{contract_config, derive_proxy_wallet, derive_safe_wallet, POLYGON};
use ports::{
    AccountBalance, AccountFill, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent,
    OpenOrder, OrderIntent, OrderIntentEnvelope, OrderIntentLifecycle, OrderStatus, Position,
};
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;

const TERMINAL_CURSOR: &str = "LTE=";
const USER_WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const USER_WS_HEALTH_INTERVAL: Duration = Duration::from_secs(1);
const USDC_SCALE: u32 = 6;
const RECONCILE_OVERLAP_US: u64 = 60_000_000;
const TERMINAL_TOMBSTONE_TTL_US: u64 = 24 * 60 * 60 * 1_000_000;
const MAX_TERMINAL_TOMBSTONES: usize = 4_096;
const PRE_SUBSCRIPTION_EVENT_CAPACITY: usize = 1_024;
const MAX_SEEN_FILLS: usize = 100_000;

type AuthenticatedClient = ClobClient<Authenticated<Normal>>;

struct FillDeduper {
    ids: HashSet<String>,
    fifo: VecDeque<String>,
    capacity: usize,
}

impl Default for FillDeduper {
    fn default() -> Self {
        Self::with_capacity(MAX_SEEN_FILLS)
    }
}

impl FillDeduper {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            ids: HashSet::with_capacity(capacity),
            fifo: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    fn insert(&mut self, fill_id: String) -> bool {
        if self.ids.contains(&fill_id) {
            return false;
        }
        if self.ids.len() >= self.capacity {
            if let Some(expired) = self.fifo.pop_front() {
                self.ids.remove(&expired);
            }
        }
        self.ids.insert(fill_id.clone());
        self.fifo.push_back(fill_id);
        true
    }

    fn contains(&self, fill_id: &str) -> bool {
        self.ids.contains(fill_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSignatureType {
    Eoa,
    Proxy,
    GnosisSafe,
    Poly1271,
}

impl WalletSignatureType {
    const fn sdk(self) -> SignatureType {
        match self {
            Self::Eoa => SignatureType::Eoa,
            Self::Proxy => SignatureType::Proxy,
            Self::GnosisSafe => SignatureType::GnosisSafe,
            Self::Poly1271 => SignatureType::Poly1271,
        }
    }
}

impl FromStr for WalletSignatureType {
    type Err = HftError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "eoa" => Ok(Self::Eoa),
            "proxy" => Ok(Self::Proxy),
            "gnosis_safe" | "gnosis-safe" | "safe" => Ok(Self::GnosisSafe),
            "poly1271" | "poly_1271" | "poly-1271" => Ok(Self::Poly1271),
            other => Err(HftError::Config(format!(
                "unsupported Polymarket signature type: {other}"
            ))),
        }
    }
}

#[derive(Clone)]
pub struct PolymarketExecutionConfig {
    pub host: String,
    pub ws_url: String,
    pub data_api_host: String,
    pub private_key: Option<SecretString>,
    pub funder: Option<String>,
    pub signature_type: WalletSignatureType,
    pub use_server_time: bool,
    pub minimum_collateral: Decimal,
}

impl Default for PolymarketExecutionConfig {
    fn default() -> Self {
        Self {
            host: "https://clob.polymarket.com".to_string(),
            ws_url: "wss://ws-subscriptions-clob.polymarket.com".to_string(),
            data_api_host: "https://data-api.polymarket.com".to_string(),
            private_key: None,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            use_server_time: true,
            minimum_collateral: Decimal::ZERO,
        }
    }
}

#[derive(Clone)]
struct TrackedOrder {
    logical_id: OrderId,
    venue_id: String,
    client_order_id: Option<String>,
    intent: OrderIntent,
    lifecycle: OrderIntentLifecycle,
    created_at: Timestamp,
    remaining_quantity: Decimal,
}

#[derive(Clone)]
struct TerminalOrder {
    order: TrackedOrder,
    terminalized_at: Timestamp,
}

/// Active orders are addressed by Monday's logical ID. Terminal aliases remain addressed by the
/// venue ID so a cancellation cannot orphan a trade that reaches CONFIRMED later.
#[derive(Clone, Default)]
struct TrackingBook {
    active: HashMap<String, TrackedOrder>,
    terminal_by_venue: HashMap<String, TerminalOrder>,
}

impl TrackingBook {
    fn is_pristine(&self) -> bool {
        self.active.is_empty() && self.terminal_by_venue.is_empty()
    }

    fn activate(&mut self, order: TrackedOrder) {
        if let Some(previous) = self
            .active
            .insert(order.logical_id.0.clone(), order.clone())
        {
            if previous.venue_id != order.venue_id {
                self.insert_terminal(previous, now_micros());
            }
        }
        self.terminal_by_venue.remove(&order.venue_id);
    }

    fn terminalize(&mut self, logical_id: &OrderId) -> Option<TrackedOrder> {
        self.terminalize_at(logical_id, now_micros())
    }

    fn terminalize_at(
        &mut self,
        logical_id: &OrderId,
        terminalized_at: Timestamp,
    ) -> Option<TrackedOrder> {
        if let Some(order) = self.active.remove(&logical_id.0) {
            self.insert_terminal(order.clone(), terminalized_at);
            return Some(order);
        }
        self.terminal_by_venue
            .values()
            .find(|terminal| {
                !terminal_expired(terminal, terminalized_at)
                    && terminal.order.logical_id == *logical_id
            })
            .map(|terminal| terminal.order.clone())
    }

    fn insert_terminal(&mut self, order: TrackedOrder, terminalized_at: Timestamp) {
        self.prune_terminal(terminalized_at);
        if !self.terminal_by_venue.contains_key(&order.venue_id)
            && self.terminal_by_venue.len() >= MAX_TERMINAL_TOMBSTONES
        {
            if let Some(oldest) = self
                .terminal_by_venue
                .iter()
                .min_by_key(|(_, terminal)| terminal.terminalized_at)
                .map(|(venue_id, _)| venue_id.clone())
            {
                self.terminal_by_venue.remove(&oldest);
            }
        }
        self.terminal_by_venue.insert(
            order.venue_id.clone(),
            TerminalOrder {
                order,
                terminalized_at,
            },
        );
    }

    fn prune_terminal(&mut self, now: Timestamp) {
        self.terminal_by_venue
            .retain(|_, terminal| !terminal_expired(terminal, now));
    }

    fn active_order(&self, logical_id: &OrderId) -> Option<&TrackedOrder> {
        self.active.get(&logical_id.0)
    }

    fn resolve_venue_id(&self, logical_id: &OrderId) -> String {
        self.active_order(logical_id)
            .map_or_else(|| logical_id.0.clone(), |order| order.venue_id.clone())
    }

    fn aliases_by_venue(&self) -> HashMap<String, TrackedOrder> {
        let now = now_micros();
        let mut aliases = self
            .terminal_by_venue
            .iter()
            .filter(|(_, terminal)| !terminal_expired(terminal, now))
            .map(|(venue_id, terminal)| (venue_id.clone(), terminal.order.clone()))
            .collect::<HashMap<_, _>>();
        aliases.extend(
            self.active
                .values()
                .cloned()
                .map(|order| (order.venue_id.clone(), order)),
        );
        aliases
    }

    fn for_venue(&self, venue_id: &str) -> Option<(TrackedOrder, bool)> {
        if let Some(order) = self
            .active
            .values()
            .find(|order| order.venue_id == venue_id)
        {
            return Some((order.clone(), false));
        }
        self.terminal_by_venue
            .get(venue_id)
            .filter(|terminal| !terminal_expired(terminal, now_micros()))
            .map(|terminal| (terminal.order.clone(), true))
    }

    fn earliest_created_at(&self) -> Option<Timestamp> {
        let now = now_micros();
        self.active
            .values()
            .chain(
                self.terminal_by_venue
                    .values()
                    .filter(|terminal| !terminal_expired(terminal, now))
                    .map(|terminal| &terminal.order),
            )
            .map(|order| order.created_at)
            .min()
    }

    fn apply_confirmed_fill(
        &mut self,
        venue_id: &str,
        quantity: Decimal,
        timestamp: Timestamp,
    ) -> HftResult<()> {
        if quantity <= Decimal::ZERO {
            return Err(HftError::Execution(format!(
                "Polymarket confirmed fill for {venue_id} has invalid quantity {quantity}"
            )));
        }
        let logical_id = self.active.iter().find_map(|(logical_id, order)| {
            (order.venue_id == venue_id).then_some(logical_id.clone())
        });
        if let Some(logical_id) = logical_id {
            let Some(order) = self.active.get_mut(&logical_id) else {
                return Err(HftError::Execution(format!(
                    "Polymarket active tracking changed while applying fill for {venue_id}"
                )));
            };
            if quantity > order.remaining_quantity {
                return Err(confirmed_fill_overflow(
                    venue_id,
                    quantity,
                    order.remaining_quantity,
                ));
            }
            order.remaining_quantity -= quantity;
            if order.remaining_quantity.is_zero() {
                self.terminalize_at(&OrderId(logical_id), timestamp);
            }
            return Ok(());
        }

        let terminal = self.terminal_by_venue.get_mut(venue_id).ok_or_else(|| {
            HftError::Execution(format!(
                "Polymarket confirmed fill references unknown order {venue_id}"
            ))
        })?;
        if terminal_expired(terminal, now_micros()) {
            return Err(HftError::Execution(format!(
                "Polymarket confirmed fill references expired order {venue_id}"
            )));
        }
        if quantity > terminal.order.remaining_quantity {
            return Err(confirmed_fill_overflow(
                venue_id,
                quantity,
                terminal.order.remaining_quantity,
            ));
        }
        terminal.order.remaining_quantity -= quantity;
        Ok(())
    }

    fn terminalize_reconciled(
        &mut self,
        logical_id: &OrderId,
        expected_venue_id: &str,
    ) -> HftResult<()> {
        if let Some(order) = self.active_order(logical_id) {
            if order.venue_id != expected_venue_id {
                return Err(stale_reconciliation_error(
                    logical_id,
                    expected_venue_id,
                    &order.venue_id,
                ));
            }
            self.terminalize(logical_id);
            return Ok(());
        }

        let terminal_matches =
            self.terminal_by_venue
                .get(expected_venue_id)
                .is_some_and(|terminal| {
                    !terminal_expired(terminal, now_micros())
                        && terminal.order.logical_id == *logical_id
                });
        if terminal_matches {
            return Ok(());
        }
        Err(HftError::Execution(format!(
            "Polymarket reconciliation lost tracked order {} at venue {expected_venue_id}",
            logical_id.0
        )))
    }

    fn update_reconciled_remaining(
        &mut self,
        logical_id: &OrderId,
        expected_venue_id: &str,
        remaining: Decimal,
    ) -> HftResult<()> {
        let order = self.active.get_mut(&logical_id.0).ok_or_else(|| {
            HftError::Execution(format!(
                "Polymarket reconciliation expected active order {} at venue {expected_venue_id}",
                logical_id.0
            ))
        })?;
        if order.venue_id != expected_venue_id {
            return Err(stale_reconciliation_error(
                logical_id,
                expected_venue_id,
                &order.venue_id,
            ));
        }
        order.remaining_quantity = remaining;
        Ok(())
    }
}

fn stale_reconciliation_error(
    logical_id: &OrderId,
    expected_venue_id: &str,
    current_venue_id: &str,
) -> HftError {
    HftError::Execution(format!(
        "Polymarket reconciliation snapshot for {} is stale: expected venue {expected_venue_id}, current venue {current_venue_id}",
        logical_id.0
    ))
}

fn confirmed_fill_overflow(venue_id: &str, quantity: Decimal, remaining: Decimal) -> HftError {
    HftError::Execution(format!(
        "Polymarket confirmed fill for {venue_id} exceeds remaining quantity: fill={quantity}, remaining={remaining}"
    ))
}

fn terminal_expired(terminal: &TerminalOrder, now: Timestamp) -> bool {
    now.saturating_sub(terminal.terminalized_at) > TERMINAL_TOMBSTONE_TTL_US
}

fn activate_submission(
    tracking: &mut TrackingBook,
    envelope: &OrderIntentEnvelope,
    logical_id: OrderId,
    venue_id: String,
    created_at: Timestamp,
) {
    tracking.activate(TrackedOrder {
        logical_id,
        venue_id,
        client_order_id: Some(envelope.client_order_id.clone()),
        intent: envelope.intent.clone(),
        lifecycle: envelope.lifecycle,
        created_at,
        remaining_quantity: envelope.intent.quantity.0,
    });
}

async fn emit_event(
    event_tx: &broadcast::Sender<ExecutionEvent>,
    pending_events: &Arc<Mutex<VecDeque<ExecutionEvent>>>,
    private_healthy: &Arc<AtomicBool>,
    event: ExecutionEvent,
) {
    let mut pending = pending_events.lock().await;
    if event_tx.receiver_count() == 0 {
        if pending.len() >= PRE_SUBSCRIPTION_EVENT_CAPACITY {
            pending.clear();
            private_healthy.store(false, Ordering::Release);
            pending.push_back(ExecutionEvent::ReconciliationRequired {
                reason: "Polymarket private events exceeded the pre-subscription backlog"
                    .to_string(),
                timestamp: now_micros(),
            });
        } else {
            pending.push_back(event);
        }
        return;
    }
    drop(pending);
    let _ = event_tx.send(event);
}

struct PreparedOrder {
    token_id: U256,
    side: PolymarketSide,
    quantity: Decimal,
    price: Decimal,
    order_type: PolymarketOrderType,
    immediate: bool,
}

pub struct PolymarketExecutionClient {
    config: PolymarketExecutionConfig,
    signer: PrivateKeySigner,
    principal: Address,
    data_client: DataClient,
    client: Option<AuthenticatedClient>,
    tracked: Arc<RwLock<TrackingBook>>,
    seen_fills: Arc<Mutex<FillDeduper>>,
    event_tx: broadcast::Sender<ExecutionEvent>,
    pending_events: Arc<Mutex<VecDeque<ExecutionEvent>>>,
    connected: Arc<AtomicBool>,
    private_healthy: Arc<AtomicBool>,
    account_recovery_required: Arc<AtomicBool>,
    account_recovery_unaccounted_fill: Arc<AtomicBool>,
    initial_account_check_complete: AtomicBool,
    submission_outcome_unknown: Arc<AtomicBool>,
    last_heartbeat: Arc<AtomicU64>,
    catch_up_after: Arc<AtomicU64>,
    private_task: Option<JoinHandle<()>>,
    replacement_sequence: u64,
}

async fn validate_account_trade_readiness(
    client: &AuthenticatedClient,
    data_client: &DataClient,
    minimum_collateral: Decimal,
) -> HftResult<()> {
    let closed_only = client.closed_only_mode().await.map_err(map_sdk_error)?;
    if closed_only.closed_only {
        return Err(HftError::Risk(
            "Polymarket account is in closed-only mode".to_string(),
        ));
    }
    let health = data_client.health().await.map_err(map_sdk_error)?;
    if !health.data.eq_ignore_ascii_case("ok") {
        return Err(HftError::Network(format!(
            "Polymarket Data API is unhealthy: {}",
            health.data
        )));
    }
    let balance = client
        .balance_allowance(
            BalanceAllowanceRequest::builder()
                .asset_type(AssetType::Collateral)
                .build(),
        )
        .await
        .map_err(map_sdk_error)?;
    let available = raw_balance_to_units(balance.balance)?;
    if available < minimum_collateral {
        return Err(HftError::InsufficientBalance(format!(
            "Polymarket collateral required={minimum_collateral}, available={available}"
        )));
    }
    let required_raw = units_to_raw(minimum_collateral)?;
    for (label, neg_risk) in [("standard V2", false), ("negative-risk V2", true)] {
        let exchange = contract_config(POLYGON, neg_risk)
            .and_then(|config| config.exchange_v2)
            .ok_or_else(|| {
                HftError::Config(format!("missing Polymarket {label} exchange contract"))
            })?;
        let allowance = balance
            .allowances
            .get(&exchange)
            .map(String::as_str)
            .unwrap_or("0")
            .parse::<U256>()
            .map_err(|error| HftError::Parse(format!("Polymarket allowance: {error}")))?;
        if allowance < required_raw {
            return Err(HftError::InsufficientBalance(format!(
                "Polymarket {label} allowance is below minimum_collateral"
            )));
        }
    }
    Ok(())
}

impl PolymarketExecutionClient {
    pub fn new(mut config: PolymarketExecutionConfig) -> HftResult<Self> {
        validate_endpoint(&config.host, &["https"])?;
        validate_endpoint(&config.ws_url, &["wss"])?;
        validate_endpoint(&config.data_api_host, &["https"])?;
        if config.minimum_collateral < Decimal::ZERO {
            return Err(HftError::Config(
                "Polymarket minimum_collateral must be non-negative".to_string(),
            ));
        }
        let key = config.private_key.take().ok_or_else(|| {
            HftError::Authentication("Polymarket private signer key is not configured".to_string())
        })?;
        let signer = PrivateKeySigner::from_str(key.expose_secret())
            .map_err(|error| {
                HftError::Authentication(format!("invalid Polymarket signer: {error}"))
            })?
            .with_chain_id(Some(POLYGON));
        let principal = execution_principal(&config, signer.address())?;
        let data_client = DataClient::new(&config.data_api_host)
            .map_err(|error| HftError::Config(format!("Polymarket Data API: {error}")))?;
        let (event_tx, _) = broadcast::channel(1_024);
        Ok(Self {
            config,
            signer,
            principal,
            data_client,
            client: None,
            tracked: Arc::new(RwLock::new(TrackingBook::default())),
            seen_fills: Arc::new(Mutex::new(FillDeduper::default())),
            event_tx,
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            connected: Arc::new(AtomicBool::new(false)),
            private_healthy: Arc::new(AtomicBool::new(false)),
            account_recovery_required: Arc::new(AtomicBool::new(false)),
            account_recovery_unaccounted_fill: Arc::new(AtomicBool::new(false)),
            initial_account_check_complete: AtomicBool::new(false),
            submission_outcome_unknown: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(0)),
            catch_up_after: Arc::new(AtomicU64::new(now_micros())),
            private_task: None,
            replacement_sequence: 0,
        })
    }

    fn authenticated(&self) -> HftResult<&AuthenticatedClient> {
        self.client.as_ref().ok_or_else(|| {
            HftError::Network("Polymarket execution client is not connected".to_string())
        })
    }

    fn ensure_ready(&self) -> HftResult<&AuthenticatedClient> {
        if self.submission_outcome_unknown.load(Ordering::Acquire) {
            return Err(HftError::Execution(
                "Polymarket submission outcome is unknown; inspect the account and restart before sending another order"
                    .to_string(),
            ));
        }
        if self.account_recovery_required.load(Ordering::Acquire) {
            return Err(HftError::Risk(
                "Polymarket account recovery is required; new orders and replacements remain disabled"
                    .to_string(),
            ));
        }
        if !self.connected.load(Ordering::Acquire) || !self.private_healthy.load(Ordering::Acquire)
        {
            return Err(HftError::Network(
                "Polymarket execution/private stream is not healthy".to_string(),
            ));
        }
        self.authenticated()
    }

    fn ensure_cancel_ready(&self) -> HftResult<&AuthenticatedClient> {
        self.ensure_cancel_transport_ready()?;
        self.authenticated()
    }

    fn ensure_cancel_transport_ready(&self) -> HftResult<()> {
        if self.connected.load(Ordering::Acquire) {
            return Ok(());
        }
        Err(HftError::Network(
            "Polymarket REST execution client is not connected".to_string(),
        ))
    }

    fn execution_ready(&self) -> bool {
        self.connected.load(Ordering::Acquire)
            && self.private_healthy.load(Ordering::Acquire)
            && !self.account_recovery_required.load(Ordering::Acquire)
    }

    fn private_stream_running(&self) -> bool {
        self.private_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    async fn require_reconciliation(&self, reason: String, sticky_submission: bool) {
        if sticky_submission {
            self.submission_outcome_unknown
                .store(true, Ordering::Release);
        }
        self.private_healthy.store(false, Ordering::Release);
        emit_event(
            &self.event_tx,
            &self.pending_events,
            &self.private_healthy,
            ExecutionEvent::ReconciliationRequired {
                reason,
                timestamp: now_micros(),
            },
        )
        .await;
    }

    async fn resolve_venue_id(&self, logical_id: &OrderId) -> String {
        self.tracked.read().await.resolve_venue_id(logical_id)
    }

    async fn submit_envelope(
        &self,
        envelope: &OrderIntentEnvelope,
        logical_id: Option<&OrderId>,
    ) -> HftResult<OrderId> {
        let client = self.ensure_ready()?;
        envelope
            .validate_pre_execution(now_micros(), None)
            .map_err(|reason| {
                HftError::InvalidOrder(format!("stale Polymarket order: {reason:?}"))
            })?;
        let prepared = self.prepare_order(client, envelope).await?;
        let metadata = envelope_metadata(&envelope.client_order_id);
        let signable = if prepared.immediate {
            client
                .market_order()
                .token_id(prepared.token_id)
                .side(prepared.side)
                .amount(
                    Amount::shares(prepared.quantity)
                        .map_err(|error| HftError::InvalidOrder(error.to_string()))?,
                )
                .price(prepared.price)
                .order_type(prepared.order_type)
                .metadata(metadata)
                .build()
                .await
                .map_err(|error| HftError::InvalidOrder(error.to_string()))?
        } else {
            client
                .limit_order()
                .token_id(prepared.token_id)
                .side(prepared.side)
                .size(prepared.quantity)
                .price(prepared.price)
                .order_type(prepared.order_type)
                .metadata(metadata)
                .build()
                .await
                .map_err(|error| HftError::InvalidOrder(error.to_string()))?
        };
        let signed = client
            .sign(&self.signer, signable)
            .await
            .map_err(|error| HftError::Authentication(format!("sign Polymarket order: {error}")))?;
        let created_at = now_micros();
        // The user stream contains only the venue order ID. Hold the write guard across POST and
        // alias installation so a private reader cannot observe an accepted order as untracked.
        // Recheck after all preparation/signing awaits so a health or recovery latch raised while
        // building the order prevents the POST.
        let mut tracking = self.tracked.write().await;
        self.ensure_ready()?;
        let response = match client.post_order(signed).await {
            Ok(response) => response,
            Err(error) => {
                let error = map_submission_error(error);
                if matches!(error, HftError::Network(_) | HftError::Timeout(_)) {
                    self.require_reconciliation(
                        format!(
                            "Polymarket order submission outcome is unknown for client_order_id={}: {error}",
                            envelope.client_order_id
                        ),
                        true,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        if !response.success {
            return Err(HftError::Exchange(response.error_msg.unwrap_or_else(
                || format!("Polymarket rejected order with status {}", response.status),
            )));
        }
        if response.order_id.trim().is_empty() {
            let error =
                HftError::Execution("Polymarket accepted an order without an order ID".to_string());
            self.require_reconciliation(
                format!(
                    "Polymarket accepted client_order_id={} without returning an order ID",
                    envelope.client_order_id
                ),
                true,
            )
            .await;
            return Err(error);
        }
        let venue_id = response.order_id;
        let logical_id = logical_id
            .cloned()
            .unwrap_or_else(|| OrderId(venue_id.clone()));
        activate_submission(
            &mut tracking,
            envelope,
            logical_id.clone(),
            venue_id,
            created_at,
        );
        Ok(logical_id)
    }

    async fn prepare_order(
        &self,
        client: &AuthenticatedClient,
        envelope: &OrderIntentEnvelope,
    ) -> HftResult<PreparedOrder> {
        validate_intent(&envelope.intent)?;
        if envelope.lifecycle.reduce_only && envelope.intent.side != Side::Sell {
            return Err(HftError::InvalidOrder(
                "Polymarket reduce_only is only valid for SELL".to_string(),
            ));
        }
        let token_id = parse_token_id(&envelope.intent.symbol)?;
        let quantity = envelope.intent.quantity.0.normalize();
        if quantity.scale() > 2 {
            return Err(HftError::InvalidOrder(
                "Polymarket share quantity supports at most 2 decimals".to_string(),
            ));
        }
        let side = sdk_side(envelope.intent.side);
        let max_slippage_bps = required_max_slippage_bps(envelope.lifecycle.max_slippage_bps)?;
        let book_request =
            polymarket_client_sdk::clob::types::request::OrderBookSummaryRequest::builder()
                .token_id(token_id)
                .build();
        let midpoint_request = MidpointRequest::builder().token_id(token_id).build();
        let (book, midpoint) = tokio::try_join!(
            async {
                client
                    .order_book(&book_request)
                    .await
                    .map_err(map_sdk_error)
            },
            async {
                client
                    .midpoint(&midpoint_request)
                    .await
                    .map_err(map_sdk_error)
            }
        )?;
        let tick = book.tick_size.as_decimal();
        let (price, order_type, immediate) = execution_price_policy(
            envelope.intent.order_type,
            envelope.intent.time_in_force,
            envelope.intent.price,
            envelope.intent.side,
            midpoint.mid,
            max_slippage_bps,
            tick,
        )?;
        if quantity < book.min_order_size {
            return Err(HftError::InvalidOrder(format!(
                "Polymarket minimum order size is {} shares",
                book.min_order_size
            )));
        }
        validate_final_order_notional(envelope, quantity, price)?;
        self.ensure_order_balance(client, token_id, side, quantity, price, book.neg_risk)
            .await?;
        Ok(PreparedOrder {
            token_id,
            side,
            quantity,
            price,
            order_type,
            immediate,
        })
    }

    async fn ensure_order_balance(
        &self,
        client: &AuthenticatedClient,
        token_id: U256,
        side: PolymarketSide,
        quantity: Decimal,
        price: Decimal,
        neg_risk: bool,
    ) -> HftResult<()> {
        let (request, required_units, label) = match side {
            PolymarketSide::Buy => (
                BalanceAllowanceRequest::builder()
                    .asset_type(AssetType::Collateral)
                    .build(),
                quantity * price,
                "USDC",
            ),
            PolymarketSide::Sell => (
                BalanceAllowanceRequest::builder()
                    .asset_type(AssetType::Conditional)
                    .token_id(token_id)
                    .build(),
                quantity,
                "conditional token",
            ),
            _ => {
                return Err(HftError::InvalidOrder(
                    "Polymarket order side is invalid".to_string(),
                ))
            }
        };
        let balance = client
            .balance_allowance(request)
            .await
            .map_err(map_sdk_error)?;
        let available = match side {
            PolymarketSide::Buy => raw_balance_to_units(balance.balance)?,
            PolymarketSide::Sell => conditional_balance_to_shares(balance.balance)?,
            _ => unreachable!("side was validated above"),
        };
        if available < required_units {
            return Err(HftError::InsufficientBalance(format!(
                "Polymarket {label}: required={required_units}, available={available}"
            )));
        }
        let exchange = contract_config(POLYGON, neg_risk)
            .and_then(|config| config.exchange_v2)
            .ok_or_else(|| {
                HftError::Config("missing Polymarket V2 exchange contract".to_string())
            })?;
        let required_raw = units_to_raw(required_units)?;
        let allowance = balance
            .allowances
            .get(&exchange)
            .map(String::as_str)
            .unwrap_or("0")
            .parse::<U256>()
            .map_err(|error| HftError::Parse(format!("Polymarket allowance: {error}")))?;
        if allowance < required_raw {
            return Err(HftError::InsufficientBalance(format!(
                "Polymarket {label} allowance is below the order amount"
            )));
        }
        Ok(())
    }

    async fn load_positions(&self) -> HftResult<Vec<DataPosition>> {
        let mut positions = Vec::new();
        let mut offset = 0;
        loop {
            let builder = PositionsRequest::builder()
                .user(self.principal)
                .size_threshold(Decimal::ZERO)
                .limit(500)
                .map_err(|error| HftError::Config(error.to_string()))?;
            let request = builder
                .offset(offset)
                .map_err(|error| HftError::Config(error.to_string()))?
                .build();
            let mut page = self
                .data_client
                .positions(&request)
                .await
                .map_err(map_sdk_error)?;
            let count = page.len();
            positions.append(&mut page);
            if count < 500 {
                break;
            }
            offset += 500;
            if offset > 10_000 {
                return Err(HftError::Execution(
                    "Polymarket position pagination exceeded API limit".to_string(),
                ));
            }
        }
        Ok(positions)
    }
}

fn validate_endpoint(value: &str, schemes: &[&str]) -> HftResult<()> {
    let url = url::Url::parse(value)
        .map_err(|error| HftError::Config(format!("invalid Polymarket endpoint: {error}")))?;
    if !schemes.contains(&url.scheme()) || url.host_str().is_none() {
        return Err(HftError::Config(format!(
            "invalid Polymarket endpoint scheme/host: {value}"
        )));
    }
    Ok(())
}

fn execution_principal(config: &PolymarketExecutionConfig, signer: Address) -> HftResult<Address> {
    let configured = config
        .funder
        .as_deref()
        .map(Address::from_str)
        .transpose()
        .map_err(|error| HftError::Config(format!("invalid Polymarket funder: {error}")))?;
    match config.signature_type {
        WalletSignatureType::Eoa => {
            if configured.is_some() {
                return Err(HftError::Config(
                    "Polymarket EOA signatures must not set funder".to_string(),
                ));
            }
            Ok(signer)
        }
        WalletSignatureType::Proxy | WalletSignatureType::GnosisSafe => {
            let derived = match config.signature_type {
                WalletSignatureType::Proxy => derive_proxy_wallet(signer, POLYGON),
                WalletSignatureType::GnosisSafe => derive_safe_wallet(signer, POLYGON),
                _ => unreachable!(),
            }
            .ok_or_else(|| {
                HftError::Config("wallet derivation unavailable on Polygon".to_string())
            })?;
            if configured.is_some_and(|value| value != derived) {
                return Err(HftError::Config(format!(
                    "Polymarket funder does not match signer-derived wallet {derived:#x}"
                )));
            }
            Ok(derived)
        }
        WalletSignatureType::Poly1271 => configured
            .filter(|value| *value != Address::ZERO)
            .ok_or_else(|| HftError::Config("Polymarket Poly1271 requires funder".to_string())),
    }
}

fn validate_intent(intent: &OrderIntent) -> HftResult<()> {
    if intent.asset_class != AssetClass::PredictionMarket
        || intent.product_type != ProductType::PredictionMarket
        || intent.target_venue != Some(VenueId::POLYMARKET)
    {
        return Err(HftError::InvalidOrder(
            "Polymarket accepts PredictionMarket intents targeting POLYMARKET".to_string(),
        ));
    }
    if intent.quantity.0 <= Decimal::ZERO {
        return Err(HftError::InvalidOrder(
            "Polymarket quantity must be positive".to_string(),
        ));
    }
    if let Some(price) = intent.price {
        if price.0 <= Decimal::ZERO || price.0 >= Decimal::ONE {
            return Err(HftError::InvalidOrder(
                "Polymarket price must be between 0 and 1".to_string(),
            ));
        }
    }
    Ok(())
}

fn parse_token_id(symbol: &Symbol) -> HftResult<U256> {
    let raw = symbol.as_str().trim();
    let token = U256::from_str(raw).map_err(|error| {
        HftError::InvalidOrder(format!("Polymarket symbol is not a token ID: {error}"))
    })?;
    if token.to_string() != raw {
        return Err(HftError::InvalidOrder(
            "Polymarket token ID must use canonical decimal form".to_string(),
        ));
    }
    Ok(token)
}

const fn sdk_side(side: Side) -> PolymarketSide {
    match side {
        Side::Buy => PolymarketSide::Buy,
        Side::Sell => PolymarketSide::Sell,
    }
}

fn hft_side(side: PolymarketSide) -> HftResult<Side> {
    match side {
        PolymarketSide::Buy => Ok(Side::Buy),
        PolymarketSide::Sell => Ok(Side::Sell),
        _ => Err(HftError::Parse(
            "Polymarket returned unknown side".to_string(),
        )),
    }
}

fn required_max_slippage_bps(value: Option<i32>) -> HftResult<i32> {
    let value = value.ok_or_else(|| {
        HftError::InvalidOrder("Polymarket live order requires signed max_slippage_bps".to_string())
    })?;
    if !(1..=10_000).contains(&value) {
        return Err(HftError::InvalidOrder(
            "Polymarket max_slippage_bps must be in 1..=10000".to_string(),
        ));
    }
    Ok(value)
}

fn execution_price_policy(
    order_type: HftOrderType,
    time_in_force: TimeInForce,
    intent_price: Option<Price>,
    side: Side,
    midpoint: Decimal,
    max_slippage_bps: i32,
    tick: Decimal,
) -> HftResult<(Decimal, PolymarketOrderType, bool)> {
    match order_type {
        HftOrderType::Limit => {
            let price = intent_price.ok_or_else(|| {
                HftError::InvalidOrder("Polymarket LIMIT requires a price".to_string())
            })?;
            validate_limit_price_against_midpoint(price.0, midpoint, side, max_slippage_bps, tick)?;
            let (order_type, immediate) = match time_in_force {
                TimeInForce::GTC => (PolymarketOrderType::GTC, false),
                TimeInForce::IOC => (PolymarketOrderType::FAK, true),
                TimeInForce::FOK => (PolymarketOrderType::FOK, true),
            };
            Ok((price.0, order_type, immediate))
        }
        HftOrderType::Market => {
            let order_type = match time_in_force {
                TimeInForce::IOC => PolymarketOrderType::FAK,
                TimeInForce::FOK => PolymarketOrderType::FOK,
                TimeInForce::GTC => {
                    return Err(HftError::InvalidOrder(
                        "Polymarket MARKET requires IOC or FOK".to_string(),
                    ))
                }
            };
            let price = slippage_price(midpoint, side, max_slippage_bps, tick)?;
            Ok((price, order_type, true))
        }
    }
}

fn validate_limit_price_against_midpoint(
    price: Decimal,
    midpoint: Decimal,
    side: Side,
    max_slippage_bps: i32,
    tick: Decimal,
) -> HftResult<()> {
    let boundary = slippage_price(midpoint, side, max_slippage_bps, tick)?;
    let outside_boundary = match side {
        Side::Buy => price > boundary,
        Side::Sell => price < boundary,
    };
    if outside_boundary {
        return Err(HftError::Risk(format!(
            "Polymarket LIMIT price {price} exceeds signed {max_slippage_bps} bps boundary {boundary} from midpoint {midpoint}"
        )));
    }
    if price <= Decimal::ZERO
        || price >= Decimal::ONE
        || tick <= Decimal::ZERO
        || !(price % tick).is_zero()
    {
        return Err(HftError::InvalidOrder(format!(
            "Polymarket LIMIT price {price} is outside the tradable tick grid {tick}"
        )));
    }
    Ok(())
}

fn validate_final_order_notional(
    envelope: &OrderIntentEnvelope,
    quantity: Decimal,
    final_price: Decimal,
) -> HftResult<()> {
    let max_order_notional = envelope.lifecycle.max_order_notional.ok_or_else(|| {
        HftError::InvalidOrder(
            "Polymarket live order requires signed max_order_notional".to_string(),
        )
    })?;
    if max_order_notional <= Decimal::ZERO {
        return Err(HftError::InvalidOrder(
            "Polymarket max_order_notional must be positive".to_string(),
        ));
    }
    let order_notional = quantity.checked_mul(final_price).ok_or_else(|| {
        HftError::InvalidOrder("Polymarket final order notional overflowed".to_string())
    })?;
    if order_notional > max_order_notional {
        return Err(HftError::Risk(format!(
            "Polymarket final order notional {order_notional} exceeds signed max_order_notional {max_order_notional}"
        )));
    }
    Ok(())
}

fn slippage_price(
    reference: Decimal,
    side: Side,
    max_slippage_bps: i32,
    tick: Decimal,
) -> HftResult<Decimal> {
    required_max_slippage_bps(Some(max_slippage_bps))?;
    if reference <= Decimal::ZERO || reference >= Decimal::ONE || tick <= Decimal::ZERO {
        return Err(HftError::InvalidOrder(
            "invalid Polymarket reference price or tick".to_string(),
        ));
    }
    let fraction = Decimal::from(max_slippage_bps) / Decimal::from(10_000);
    let raw = match side {
        Side::Buy => reference * (Decimal::ONE + fraction),
        Side::Sell => reference * (Decimal::ONE - fraction),
    };
    let aligned = (match side {
        Side::Buy => (raw / tick).floor() * tick,
        Side::Sell => (raw / tick).ceil() * tick,
    })
    .max(tick)
    .min(Decimal::ONE - tick);
    if aligned <= Decimal::ZERO || aligned >= Decimal::ONE {
        return Err(HftError::InvalidOrder(
            "Polymarket slippage bound is outside the tradable range".to_string(),
        ));
    }
    Ok(aligned)
}

fn polymarket_taker_fee(
    shares: Decimal,
    price: Decimal,
    fee_rate_bps: Decimal,
) -> HftResult<Decimal> {
    if shares <= Decimal::ZERO || price <= Decimal::ZERO || price >= Decimal::ONE {
        return Err(HftError::Execution(
            "invalid Polymarket taker fill quantity or price".to_string(),
        ));
    }
    if fee_rate_bps < Decimal::ZERO
        || fee_rate_bps > Decimal::from(10_000)
        || !fee_rate_bps.fract().is_zero()
    {
        return Err(HftError::Execution(format!(
            "invalid Polymarket taker fee_rate_bps: {fee_rate_bps}"
        )));
    }
    let fee = shares * (fee_rate_bps / Decimal::from(10_000)) * price * (Decimal::ONE - price);
    if fee < Decimal::new(1, 5) {
        return Ok(Decimal::ZERO);
    }
    Ok(fee.round_dp(5))
}

/// FeeCharged must immediately follow its Fill and share the same fill ID. Dedupe the pair as one
/// accounting unit so a REST/private overlap can neither duplicate the fee nor drop it alone.
fn dedupe_fill_event_groups(
    events: Vec<ExecutionEvent>,
    seen_fills: &mut FillDeduper,
) -> Vec<ExecutionEvent> {
    let mut output = Vec::with_capacity(events.len());
    let mut preceding_fill: Option<(String, bool)> = None;
    for event in events {
        match &event {
            ExecutionEvent::Fill { fill_id, .. } => {
                let is_new = seen_fills.insert(fill_id.clone());
                preceding_fill = Some((fill_id.clone(), is_new));
                if is_new {
                    output.push(event);
                }
            }
            ExecutionEvent::FeeCharged { fill_id, .. } => {
                let emit = preceding_fill
                    .take()
                    .is_some_and(|(preceding_id, is_new)| preceding_id == *fill_id && is_new);
                if emit {
                    output.push(event);
                }
            }
            _ => {
                preceding_fill = None;
                output.push(event);
            }
        }
    }
    output
}

fn envelope_metadata(client_order_id: &str) -> B256 {
    B256::from_slice(&Sha256::digest(client_order_id.as_bytes()))
}

fn units_to_raw(value: Decimal) -> HftResult<U256> {
    if value < Decimal::ZERO {
        return Err(HftError::Parse("negative Polymarket amount".to_string()));
    }
    let scaled = (value * Decimal::from(10_u64.pow(USDC_SCALE))).ceil();
    U256::from_str(&scaled.to_string())
        .map_err(|error| HftError::Parse(format!("Polymarket amount is out of range: {error}")))
}

fn raw_balance_to_units(value: Decimal) -> HftResult<Decimal> {
    if value < Decimal::ZERO || !value.fract().is_zero() {
        return Err(HftError::Parse(format!(
            "Polymarket returned invalid raw balance: {value}"
        )));
    }
    Ok(value / Decimal::from(10_u64.pow(USDC_SCALE)))
}

fn conditional_balance_to_shares(value: Decimal) -> HftResult<Decimal> {
    if value < Decimal::ZERO {
        return Err(HftError::Parse(format!(
            "Polymarket returned a negative conditional-token balance: {value}"
        )));
    }
    if value.scale() == 0 {
        Ok(value / Decimal::from(10_u64.pow(USDC_SCALE)))
    } else {
        Ok(value)
    }
}

fn map_sdk_error(error: polymarket_client_sdk::error::Error) -> HftError {
    match error.kind() {
        SdkErrorKind::Geoblock => HftError::Risk(error.to_string()),
        SdkErrorKind::Validation => HftError::InvalidOrder(error.to_string()),
        SdkErrorKind::Status => {
            if let Some(status) = error.downcast_ref::<SdkStatus>() {
                match status.status_code.as_u16() {
                    401 | 403 => HftError::Authentication(error.to_string()),
                    429 => HftError::RateLimit(error.to_string()),
                    404 => HftError::OrderNotFound(error.to_string()),
                    _ if status.status_code.is_server_error() => {
                        HftError::Network(error.to_string())
                    }
                    _ => HftError::Exchange(error.to_string()),
                }
            } else {
                HftError::Exchange(error.to_string())
            }
        }
        SdkErrorKind::WebSocket | SdkErrorKind::Internal | SdkErrorKind::Synchronization => {
            HftError::Network(error.to_string())
        }
        _ => HftError::Exchange(error.to_string()),
    }
}

fn map_submission_error(error: polymarket_client_sdk::error::Error) -> HftError {
    if let Some(status) = error.downcast_ref::<SdkStatus>() {
        return match status.status_code.as_u16() {
            400 | 404 | 409 | 422 => HftError::Exchange(error.to_string()),
            401 | 403 => HftError::Authentication(error.to_string()),
            429 => HftError::RateLimit(error.to_string()),
            _ if status.status_code.is_client_error() => HftError::Exchange(error.to_string()),
            _ => HftError::Network(format!("Polymarket submission outcome unknown: {error}")),
        };
    }
    match error.kind() {
        SdkErrorKind::Validation | SdkErrorKind::Geoblock => map_sdk_error(error),
        _ => HftError::Network(format!("Polymarket submission outcome unknown: {error}")),
    }
}

fn micros(date: chrono::DateTime<chrono::Utc>) -> HftResult<u64> {
    u64::try_from(date.timestamp_micros())
        .map_err(|_| HftError::Parse("Polymarket timestamp predates Unix epoch".to_string()))
}

fn wire_micros(value: Option<i64>) -> Timestamp {
    let value = value.and_then(|value| u64::try_from(value).ok());
    match value {
        Some(value @ 0..=99_999_999_999) => value.saturating_mul(1_000_000),
        Some(value @ 100_000_000_000..=99_999_999_999_999) => value.saturating_mul(1_000),
        Some(value) => value,
        None => now_micros(),
    }
}

async fn load_all_orders(client: &AuthenticatedClient) -> HftResult<Vec<OpenOrderResponse>> {
    let mut orders = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    loop {
        let page = client
            .orders(&OrdersRequest::default(), cursor.clone())
            .await
            .map_err(map_sdk_error)?;
        orders.extend(page.data);
        if page.next_cursor == TERMINAL_CURSOR {
            break;
        }
        if page.next_cursor.is_empty() || !seen.insert(page.next_cursor.clone()) {
            return Err(HftError::Execution(
                "Polymarket order pagination returned an invalid cursor".to_string(),
            ));
        }
        cursor = Some(page.next_cursor);
    }
    Ok(orders)
}

async fn load_all_trades(
    client: &AuthenticatedClient,
    request: &TradesRequest,
) -> HftResult<Vec<TradeResponse>> {
    let mut trades = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    loop {
        let page = client
            .trades(request, cursor.clone())
            .await
            .map_err(map_sdk_error)?;
        trades.extend(page.data);
        if page.next_cursor == TERMINAL_CURSOR {
            break;
        }
        if page.next_cursor.is_empty() || !seen.insert(page.next_cursor.clone()) {
            return Err(HftError::Execution(
                "Polymarket trade pagination returned an invalid cursor".to_string(),
            ));
        }
        cursor = Some(page.next_cursor);
    }
    Ok(trades)
}

fn hft_order_type(value: &PolymarketOrderType) -> HftResult<HftOrderType> {
    match value {
        PolymarketOrderType::GTC | PolymarketOrderType::GTD => Ok(HftOrderType::Limit),
        PolymarketOrderType::FAK | PolymarketOrderType::FOK => Ok(HftOrderType::Market),
        PolymarketOrderType::Unknown(value) => Err(HftError::Parse(format!(
            "unknown Polymarket order type: {value}"
        ))),
        _ => Err(HftError::Parse(
            "unsupported Polymarket order type".to_string(),
        )),
    }
}

fn open_order(
    order: OpenOrderResponse,
    aliases: &HashMap<String, TrackedOrder>,
) -> HftResult<Option<OpenOrder>> {
    let remaining = (order.original_size - order.size_matched).max(Decimal::ZERO);
    let status = match &order.status {
        OrderStatusType::Live | OrderStatusType::Unmatched
            if order.size_matched > Decimal::ZERO =>
        {
            OrderStatus::PartiallyFilled
        }
        OrderStatusType::Live | OrderStatusType::Unmatched => OrderStatus::Accepted,
        OrderStatusType::Matched if remaining > Decimal::ZERO => OrderStatus::PartiallyFilled,
        OrderStatusType::Matched | OrderStatusType::Canceled => return Ok(None),
        OrderStatusType::Delayed => {
            return Err(HftError::Execution(format!(
                "Polymarket order {} is delayed; reconciliation required",
                order.id
            )))
        }
        OrderStatusType::Unknown(value) => {
            return Err(HftError::Parse(format!(
                "unknown Polymarket order status: {value}"
            )))
        }
        _ => {
            return Err(HftError::Parse(
                "unsupported Polymarket order status".to_string(),
            ))
        }
    };
    let tracked = aliases.get(&order.id);
    let logical_id = tracked
        .map(|tracked| tracked.logical_id.clone())
        .unwrap_or_else(|| OrderId(order.id));
    Ok(Some(OpenOrder {
        order_id: logical_id,
        client_order_id: tracked.and_then(|tracked| tracked.client_order_id.clone()),
        symbol: Symbol::new(order.asset_id.to_string()),
        side: hft_side(order.side)?,
        order_type: hft_order_type(&order.order_type)?,
        original_quantity: Quantity(order.original_size),
        remaining_quantity: Quantity(remaining),
        filled_quantity: Quantity(order.size_matched),
        price: Some(Price(order.price)),
        status,
        created_at: micros(order.created_at)?,
        updated_at: now_micros(),
    }))
}

struct ResolvedAccountFill {
    venue_id: String,
    is_taker: bool,
    fill: AccountFill,
}

fn rest_trade_settlement_pending(status: &TradeStatusType) -> bool {
    matches!(
        status,
        TradeStatusType::Matched | TradeStatusType::Mined | TradeStatusType::Retrying
    )
}

fn private_trade_may_have_unaccounted_fill(status: &TradeMessageStatus) -> bool {
    matches!(
        status,
        TradeMessageStatus::Matched
            | TradeMessageStatus::Mined
            | TradeMessageStatus::Retrying
            | TradeMessageStatus::Confirmed
    )
}

fn rest_settlement_pending_error(trade: &TradeResponse) -> HftError {
    HftError::Execution(format!(
        "Polymarket trade {} remains settlement-pending in status {}; retry strict REST reconciliation after it reaches CONFIRMED or FAILED",
        trade.id, trade.status
    ))
}

fn failed_account_order_venues(
    trade: &TradeResponse,
    api_key: polymarket_client_sdk::auth::ApiKey,
    aliases: &HashMap<String, TrackedOrder>,
) -> HftResult<Vec<String>> {
    let mut venues = Vec::new();
    match &trade.trader_side {
        TraderSide::Taker => {
            if let Some(tracked) = aliases.get(&trade.taker_order_id) {
                validate_tracked_identity(
                    tracked,
                    trade.asset_id,
                    hft_side(trade.side)?,
                    "REST failed-trade",
                )?;
                venues.push(trade.taker_order_id.clone());
            }
        }
        TraderSide::Maker => {
            for maker in trade
                .maker_orders
                .iter()
                .filter(|maker| maker.owner == api_key)
            {
                if let Some(tracked) = aliases.get(&maker.order_id) {
                    validate_tracked_identity(
                        tracked,
                        maker.asset_id,
                        hft_side(maker.side)?,
                        "REST failed-trade",
                    )?;
                    venues.push(maker.order_id.clone());
                }
            }
        }
        TraderSide::Unknown(value) => {
            return Err(HftError::Parse(format!(
                "unknown Polymarket trader side for failed trade {}: {value}",
                trade.id
            )))
        }
        _ => {
            return Err(HftError::Parse(format!(
                "unsupported Polymarket trader side for failed trade {}",
                trade.id
            )))
        }
    }
    Ok(venues)
}

fn resolved_account_fills_from_trade(
    trade: &TradeResponse,
    api_key: polymarket_client_sdk::auth::ApiKey,
    aliases: &HashMap<String, TrackedOrder>,
) -> HftResult<Vec<ResolvedAccountFill>> {
    if matches!(trade.status, TradeStatusType::Unknown(_)) {
        return Err(HftError::Parse(format!(
            "unknown Polymarket trade status for {}",
            trade.id
        )));
    }
    if rest_trade_settlement_pending(&trade.status) {
        return Err(rest_settlement_pending_error(trade));
    }
    match trade.status {
        TradeStatusType::Confirmed => {}
        TradeStatusType::Failed => return Ok(Vec::new()),
        _ => {
            return Err(HftError::Parse(format!(
                "unsupported Polymarket trade status for {}",
                trade.id
            )))
        }
    }
    let timestamp = micros(trade.match_time)?;
    let mut fills = Vec::new();
    match trade.trader_side {
        TraderSide::Taker => {
            let venue_id = trade.taker_order_id.clone();
            let fee = polymarket_taker_fee(trade.size, trade.price, trade.fee_rate_bps)?;
            let logical = aliases
                .get(&venue_id)
                .map(|order| order.logical_id.clone())
                .unwrap_or_else(|| OrderId(venue_id.clone()));
            fills.push(ResolvedAccountFill {
                venue_id: venue_id.clone(),
                is_taker: true,
                fill: AccountFill {
                    fill_id: format!("{}:{venue_id}", trade.id),
                    order_id: logical,
                    symbol: Symbol::new(trade.asset_id.to_string()),
                    side: hft_side(trade.side)?,
                    price: Price(trade.price),
                    quantity: Quantity(trade.size),
                    fee: Some(fee),
                    timestamp,
                },
            });
        }
        TraderSide::Maker => {
            for maker in trade
                .maker_orders
                .iter()
                .filter(|maker| maker.owner == api_key)
            {
                let venue_id = maker.order_id.clone();
                let logical = aliases
                    .get(&venue_id)
                    .map(|order| order.logical_id.clone())
                    .unwrap_or_else(|| OrderId(venue_id.clone()));
                fills.push(ResolvedAccountFill {
                    venue_id: venue_id.clone(),
                    is_taker: false,
                    fill: AccountFill {
                        fill_id: format!("{}:{venue_id}", trade.id),
                        order_id: logical,
                        symbol: Symbol::new(maker.asset_id.to_string()),
                        side: hft_side(maker.side)?,
                        price: Price(maker.price),
                        quantity: Quantity(maker.matched_amount),
                        fee: Some(Decimal::ZERO),
                        timestamp,
                    },
                });
            }
        }
        TraderSide::Unknown(ref value) => {
            return Err(HftError::Parse(format!(
                "unknown Polymarket trader side: {value}"
            )))
        }
        _ => {
            return Err(HftError::Parse(
                "unsupported Polymarket trader side".to_string(),
            ))
        }
    }
    Ok(fills)
}

fn account_fills_from_trade(
    trade: &TradeResponse,
    api_key: polymarket_client_sdk::auth::ApiKey,
    aliases: &HashMap<String, TrackedOrder>,
) -> HftResult<Vec<AccountFill>> {
    Ok(resolved_account_fills_from_trade(trade, api_key, aliases)?
        .into_iter()
        .map(|resolved| resolved.fill)
        .collect())
}

fn validate_tracked_identity(
    tracked: &TrackedOrder,
    asset_id: U256,
    side: Side,
    source: &str,
) -> HftResult<()> {
    let expected_token = parse_token_id(&tracked.intent.symbol)?;
    if asset_id != expected_token || side != tracked.intent.side {
        return Err(HftError::Execution(format!(
            "Polymarket {source} identity mismatch for {}",
            tracked.venue_id
        )));
    }
    Ok(())
}

#[derive(Clone)]
struct ConfirmedFillUpdate {
    fill_id: String,
    venue_id: String,
    quantity: Decimal,
    timestamp: Timestamp,
}

struct AccountActivity {
    events: Vec<ExecutionEvent>,
    fill_updates: Vec<ConfirmedFillUpdate>,
    failed_orders: HashMap<String, Timestamp>,
}

/// Validate a complete account-level REST slice before any dedupe state is committed. This is what
/// prevents an empty local tracking map from being mistaken for a clean private-stream recovery.
fn account_activity_events(
    open_orders: &[OpenOrderResponse],
    trades: &[TradeResponse],
    tracking: &TrackingBook,
    api_key: polymarket_client_sdk::auth::ApiKey,
    recovery_unaccounted_fill: Option<&AtomicBool>,
) -> HftResult<AccountActivity> {
    let aliases = tracking.aliases_by_venue();
    for order in open_orders {
        let tracked = aliases.get(&order.id).ok_or_else(|| {
            HftError::Execution(format!(
                "Polymarket account has unmapped open order {}",
                order.id
            ))
        })?;
        if tracking.terminal_by_venue.contains_key(&order.id) {
            return Err(HftError::Execution(format!(
                "Polymarket terminal order {} is still open",
                order.id
            )));
        }
        validate_tracked_identity(
            tracked,
            order.asset_id,
            hft_side(order.side)?,
            "REST open-order",
        )?;
    }

    let mut events = Vec::new();
    let mut fill_updates = Vec::new();
    let mut failed_orders: HashMap<String, Timestamp> = HashMap::new();
    for trade in trades {
        if rest_trade_settlement_pending(&trade.status) {
            if let Some(unaccounted_fill) = recovery_unaccounted_fill {
                unaccounted_fill.store(true, Ordering::Release);
            }
            return Err(rest_settlement_pending_error(trade));
        }
        if matches!(trade.status, TradeStatusType::Failed) {
            let timestamp = micros(trade.last_update)?;
            for venue_id in failed_account_order_venues(trade, api_key, &aliases)? {
                failed_orders
                    .entry(venue_id)
                    .and_modify(|previous| *previous = (*previous).max(timestamp))
                    .or_insert(timestamp);
            }
            continue;
        }
        let fills = resolved_account_fills_from_trade(trade, api_key, &aliases)?;
        if matches!(trade.status, TradeStatusType::Confirmed) && fills.is_empty() {
            if let Some(unaccounted_fill) = recovery_unaccounted_fill {
                unaccounted_fill.store(true, Ordering::Release);
            }
            return Err(HftError::Execution(format!(
                "Polymarket account has confirmed trade {} without an identifiable account order",
                trade.id
            )));
        }
        for resolved in fills {
            let tracked = match aliases.get(&resolved.venue_id) {
                Some(tracked) => tracked,
                None => {
                    if let Some(unaccounted_fill) = recovery_unaccounted_fill {
                        unaccounted_fill.store(true, Ordering::Release);
                    }
                    return Err(HftError::Execution(format!(
                        "Polymarket account has confirmed fill {} for unmapped order {}",
                        resolved.fill.fill_id, resolved.venue_id
                    )));
                }
            };
            validate_tracked_identity(
                tracked,
                parse_token_id(&resolved.fill.symbol)?,
                resolved.fill.side,
                "REST confirmed-fill",
            )?;
            fill_updates.push(ConfirmedFillUpdate {
                fill_id: resolved.fill.fill_id.clone(),
                venue_id: resolved.venue_id.clone(),
                quantity: resolved.fill.quantity.0,
                timestamp: resolved.fill.timestamp,
            });
            let fee_event = resolved.is_taker.then(|| ExecutionEvent::FeeCharged {
                order_id: resolved.fill.order_id.clone(),
                amount: resolved.fill.fee.unwrap_or(Decimal::ZERO),
                timestamp: resolved.fill.timestamp,
                fill_id: resolved.fill.fill_id.clone(),
            });
            events.push(ExecutionEvent::Fill {
                order_id: resolved.fill.order_id,
                price: resolved.fill.price,
                quantity: resolved.fill.quantity,
                timestamp: resolved.fill.timestamp,
                fill_id: resolved.fill.fill_id,
            });
            if let Some(fee_event) = fee_event {
                events.push(fee_event);
            }
        }
    }
    // A private MATCHED order update may have already moved the adapter alias to a tombstone while
    // the corresponding trade later settles as FAILED. The complete account open-order slice above
    // proves that the old venue order did not reopen. Stage a no-fill terminal event so the worker
    // and OMS can close the logical order before private health is advertised as recovered. Do not
    // close a newer replacement that reuses the logical ID.
    for (venue_id, timestamp) in &failed_orders {
        let Some(terminal) = tracking.terminal_by_venue.get(venue_id) else {
            continue;
        };
        if tracking.active.contains_key(&terminal.order.logical_id.0) {
            continue;
        }
        events.push(ExecutionEvent::OrderCanceled {
            order_id: terminal.order.logical_id.clone(),
            timestamp: *timestamp,
        });
    }
    Ok(AccountActivity {
        events,
        fill_updates,
        failed_orders,
    })
}

async fn rest_catch_up(
    client: &AuthenticatedClient,
    tracked: &Arc<RwLock<TrackingBook>>,
    seen_fills: &Arc<Mutex<FillDeduper>>,
    catch_up_after: &Arc<AtomicU64>,
    recovery_unaccounted_fill: Option<&AtomicBool>,
) -> HftResult<Vec<ExecutionEvent>> {
    let catch_up_started_at = now_micros();
    let tracked_snapshot = tracked.read().await.clone();
    let api_key = client.credentials().key();
    let after_us = tracked_snapshot
        .earliest_created_at()
        .unwrap_or_else(|| catch_up_after.load(Ordering::Acquire))
        .min(catch_up_after.load(Ordering::Acquire))
        .saturating_sub(RECONCILE_OVERLAP_US);
    let after = i64::try_from(after_us / 1_000_000).unwrap_or(i64::MAX);
    let open_orders = load_all_orders(client).await?;
    let trades = load_all_trades(client, &TradesRequest::builder().after(after).build()).await?;
    let mut activity = account_activity_events(
        &open_orders,
        &trades,
        &tracked_snapshot,
        api_key,
        recovery_unaccounted_fill,
    )?;
    let mut terminalize = Vec::new();
    let mut remaining_updates = Vec::new();
    for order in tracked_snapshot.active.values() {
        let venue_order = client.order(&order.venue_id).await.map_err(map_sdk_error)?;
        validate_tracked_identity(
            order,
            venue_order.asset_id,
            hft_side(venue_order.side)?,
            "REST order-status",
        )?;
        if venue_order.original_size <= Decimal::ZERO
            || venue_order.size_matched < Decimal::ZERO
            || venue_order.size_matched > venue_order.original_size
        {
            return Err(HftError::Execution(format!(
                "Polymarket order {} returned invalid fill progress",
                order.venue_id
            )));
        }
        let remaining = venue_order.original_size - venue_order.size_matched;
        match venue_order.status {
            OrderStatusType::Canceled => {
                activity.events.push(ExecutionEvent::OrderCanceled {
                    order_id: order.logical_id.clone(),
                    timestamp: now_micros(),
                });
                terminalize.push((order.logical_id.clone(), order.venue_id.clone()));
            }
            OrderStatusType::Matched => {
                if remaining > Decimal::ZERO || activity.failed_orders.contains_key(&order.venue_id)
                {
                    activity.events.push(ExecutionEvent::OrderCanceled {
                        order_id: order.logical_id.clone(),
                        timestamp: activity
                            .failed_orders
                            .get(&order.venue_id)
                            .copied()
                            .unwrap_or_else(now_micros),
                    });
                }
                terminalize.push((order.logical_id.clone(), order.venue_id.clone()));
            }
            OrderStatusType::Live | OrderStatusType::Unmatched => {
                remaining_updates.push((
                    order.logical_id.clone(),
                    order.venue_id.clone(),
                    remaining,
                ));
            }
            OrderStatusType::Delayed | OrderStatusType::Unknown(_) => {
                return Err(HftError::Execution(format!(
                    "Polymarket order {} cannot be reconciled from status {}",
                    order.venue_id, venue_order.status
                )))
            }
            _ => {
                return Err(HftError::Execution(format!(
                    "Polymarket order {} returned unsupported status {}",
                    order.venue_id, venue_order.status
                )))
            }
        }
    }

    // Commit fill accounting, dedupe, and terminal transitions only after every account-level
    // check succeeded. Apply to a clone first so a later overfill cannot partially mutate state.
    let mut seen = seen_fills.lock().await;
    let mut pending_fill_ids = HashSet::new();
    let mut tracking = tracked.write().await;
    let mut prospective = tracking.clone();
    for update in &activity.fill_updates {
        if !seen.contains(&update.fill_id) && pending_fill_ids.insert(update.fill_id.as_str()) {
            prospective.apply_confirmed_fill(
                &update.venue_id,
                update.quantity,
                update.timestamp,
            )?;
        }
    }
    for (logical_id, expected_venue_id) in terminalize {
        prospective.terminalize_reconciled(&logical_id, &expected_venue_id)?;
    }
    for (logical_id, expected_venue_id, remaining) in remaining_updates {
        prospective.update_reconciled_remaining(&logical_id, &expected_venue_id, remaining)?;
    }
    activity.events = dedupe_fill_event_groups(activity.events, &mut seen);
    *tracking = prospective;
    drop(tracking);
    drop(seen);
    catch_up_after.store(catch_up_started_at, Ordering::Release);
    Ok(activity.events)
}

fn may_bootstrap_account_recovery(
    tracking: &TrackingBook,
    initial_account_check_complete: bool,
    recovery_required: bool,
) -> bool {
    tracking.is_pristine() && (!initial_account_check_complete || recovery_required)
}

fn recovery_orders_have_unaccounted_fill(open_orders: &[OpenOrderResponse]) -> bool {
    open_orders
        .iter()
        .any(|order| order.size_matched > Decimal::ZERO)
}

/// A recovery latch can clear only after the normal strict account-wide catch-up succeeds and a
/// second complete open-order snapshot confirms that no order appeared during that catch-up.
struct AccountRecoveryContext<'a> {
    data_client: &'a DataClient,
    minimum_collateral: Decimal,
    tracked: &'a Arc<RwLock<TrackingBook>>,
    seen_fills: &'a Arc<Mutex<FillDeduper>>,
    catch_up_after: &'a Arc<AtomicU64>,
    required: &'a Arc<AtomicBool>,
    unaccounted_fill: &'a Arc<AtomicBool>,
}

async fn rest_catch_up_and_confirm_recovery(
    client: &AuthenticatedClient,
    recovery: AccountRecoveryContext<'_>,
) -> HftResult<Vec<ExecutionEvent>> {
    let recovery_fill_latch = recovery
        .required
        .load(Ordering::Acquire)
        .then_some(recovery.unaccounted_fill.as_ref());
    let events = rest_catch_up(
        client,
        recovery.tracked,
        recovery.seen_fills,
        recovery.catch_up_after,
        recovery_fill_latch,
    )
    .await?;
    if recovery.required.load(Ordering::Acquire) {
        let final_open_orders = load_all_orders(client).await?;
        if final_open_orders.is_empty() {
            validate_account_trade_readiness(
                client,
                recovery.data_client,
                recovery.minimum_collateral,
            )
            .await?;
        }
        confirm_account_recovery_open_orders(
            &final_open_orders,
            recovery.required.as_ref(),
            recovery.unaccounted_fill.as_ref(),
        )?;
    }
    Ok(events)
}

fn confirm_account_recovery_open_orders(
    final_open_orders: &[OpenOrderResponse],
    account_recovery_required: &AtomicBool,
    account_recovery_unaccounted_fill: &AtomicBool,
) -> HftResult<()> {
    if !final_open_orders.is_empty() {
        return Err(HftError::Execution(format!(
            "Polymarket account recovery still has {} exchange-only open order(s)",
            final_open_orders.len()
        )));
    }
    if account_recovery_unaccounted_fill.load(Ordering::Acquire) {
        return Err(HftError::Execution(
            "Polymarket account recovery found pre-start fills; restore/reconcile portfolio state before enabling new exposure"
                .to_string(),
        ));
    }
    account_recovery_required.store(false, Ordering::Release);
    Ok(())
}

async fn private_order_events(
    order: OrderMessage,
    tracked: &Arc<RwLock<TrackingBook>>,
) -> HftResult<Vec<ExecutionEvent>> {
    let mut tracking = tracked.write().await;
    let Some((tracked, terminal)) = tracking.for_venue(&order.id) else {
        return Err(HftError::Execution(format!(
            "Polymarket user stream reported untracked order {}",
            order.id
        )));
    };
    let expected_token = parse_token_id(&tracked.intent.symbol)?;
    if order.asset_id != expected_token || hft_side(order.side)? != tracked.intent.side {
        return Err(HftError::Execution(format!(
            "Polymarket private order identity mismatch for {}",
            order.id
        )));
    }
    if matches!(order.msg_type, Some(OrderMessageType::Unknown(_)))
        || matches!(
            order.status,
            Some(OrderStatusType::Delayed | OrderStatusType::Unknown(_))
        )
    {
        return Err(HftError::Execution(format!(
            "unknown/delayed Polymarket private order state for {}",
            order.id
        )));
    }
    // A terminal alias exists only to attribute delayed terminal traffic. A placement/live event
    // for it means the supposedly closed venue order may still be open and must fail closed.
    if terminal {
        if matches!(order.msg_type, Some(OrderMessageType::Placement))
            || matches!(
                order.status,
                Some(OrderStatusType::Live | OrderStatusType::Unmatched)
            )
        {
            return Err(HftError::Execution(format!(
                "Polymarket terminal order {} returned to an open state",
                order.id
            )));
        }
        if matches!(order.msg_type, Some(OrderMessageType::Cancellation))
            || matches!(
                order.status,
                Some(OrderStatusType::Canceled | OrderStatusType::Matched)
            )
        {
            return Ok(Vec::new());
        }
        return Err(HftError::Execution(format!(
            "Polymarket terminal order {} reported an ambiguous non-terminal update",
            order.id
        )));
    }
    let timestamp = wire_micros(order.timestamp);
    if matches!(order.msg_type, Some(OrderMessageType::Cancellation))
        || matches!(order.status, Some(OrderStatusType::Canceled))
    {
        tracking.terminalize_at(&tracked.logical_id, timestamp);
        return Ok(vec![ExecutionEvent::OrderCanceled {
            order_id: tracked.logical_id,
            timestamp,
        }]);
    }
    let progress = match (order.original_size, order.size_matched) {
        (Some(original), Some(matched))
            if original > Decimal::ZERO && matched >= Decimal::ZERO && matched <= original =>
        {
            Some((original, matched))
        }
        (None, None) => None,
        _ => {
            return Err(HftError::Execution(format!(
                "Polymarket private order {} has invalid/incomplete fill progress",
                order.id
            )))
        }
    };
    if matches!(order.status, Some(OrderStatusType::Matched)) {
        let (original, matched) = progress.ok_or_else(|| {
            HftError::Execution(format!(
                "Polymarket MATCHED order {} omitted fill progress",
                order.id
            ))
        })?;
        tracking.terminalize_at(&tracked.logical_id, timestamp);
        if matched < original {
            return Ok(vec![ExecutionEvent::OrderCanceled {
                order_id: tracked.logical_id,
                timestamp,
            }]);
        }
        return Ok(Vec::new());
    }
    if progress.is_some_and(|(original, matched)| matched == original) {
        tracking.terminalize_at(&tracked.logical_id, timestamp);
        return Ok(Vec::new());
    }
    if matches!(order.msg_type, Some(OrderMessageType::Placement))
        || matches!(
            order.status,
            Some(OrderStatusType::Live | OrderStatusType::Unmatched)
        )
    {
        return Ok(vec![ExecutionEvent::OrderAck {
            order_id: tracked.logical_id,
            timestamp,
        }]);
    }
    Ok(Vec::new())
}

async fn private_trade_events(
    trade: TradeMessage,
    tracked: &Arc<RwLock<TrackingBook>>,
    seen_fills: &Arc<Mutex<FillDeduper>>,
) -> HftResult<Vec<ExecutionEvent>> {
    match &trade.status {
        TradeMessageStatus::Matched
        | TradeMessageStatus::Mined
        | TradeMessageStatus::Retrying => {
            return Err(HftError::Execution(format!(
                "Polymarket private trade {} remains settlement-pending in status {:?}; strict REST reconciliation required",
                trade.id, trade.status
            )))
        }
        TradeMessageStatus::Failed => {
            return Err(HftError::Execution(format!(
                "Polymarket private trade {} reached FAILED; strict REST order/account reconciliation required",
                trade.id
            )))
        }
        TradeMessageStatus::Unknown(_) => {
            return Err(HftError::Execution(format!(
                "Polymarket private trade {} has unknown status",
                trade.id
            )))
        }
        TradeMessageStatus::Confirmed => {}
        _ => {
            return Err(HftError::Execution(format!(
                "Polymarket private trade {} has an unsupported status",
                trade.id
            )))
        }
    }
    let tracked_snapshot = tracked.read().await.clone();
    let aliases = tracked_snapshot.aliases_by_venue();
    let timestamp = wire_micros(trade.matchtime.or(trade.timestamp).or(trade.last_update));
    let mut candidates = Vec::new();
    if let Some(venue_id) = trade.taker_order_id.as_ref() {
        if let Some(order) = aliases.get(venue_id) {
            let expected_token = parse_token_id(&order.intent.symbol)?;
            if trade.asset_id != expected_token || hft_side(trade.side)? != order.intent.side {
                return Err(HftError::Execution(format!(
                    "Polymarket private taker fill identity mismatch for {venue_id}"
                )));
            }
            let fee_rate_bps = trade.fee_rate_bps.ok_or_else(|| {
                HftError::Execution(format!(
                    "Polymarket confirmed taker trade {} is missing fee_rate_bps",
                    trade.id
                ))
            })?;
            let fee = polymarket_taker_fee(trade.size, trade.price, fee_rate_bps)?;
            candidates.push((
                order.logical_id.clone(),
                venue_id.clone(),
                trade.price,
                trade.size,
                Some(fee),
            ));
        }
    }
    for maker in &trade.maker_orders {
        if let Some(order) = aliases.get(&maker.order_id) {
            if maker.matched_amount <= Decimal::ZERO
                || maker.price <= Decimal::ZERO
                || maker.price >= Decimal::ONE
            {
                return Err(HftError::Execution(format!(
                    "Polymarket confirmed maker trade {} has invalid quantity/price",
                    trade.id
                )));
            }
            let expected_token = parse_token_id(&order.intent.symbol)?;
            let maker_side = match hft_side(trade.side)? {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            };
            if maker.asset_id != expected_token || maker_side != order.intent.side {
                return Err(HftError::Execution(format!(
                    "Polymarket private maker fill identity mismatch for {}",
                    maker.order_id
                )));
            }
            candidates.push((
                order.logical_id.clone(),
                maker.order_id.clone(),
                maker.price,
                maker.matched_amount,
                None,
            ));
        }
    }
    if candidates.is_empty() {
        return Err(HftError::Execution(format!(
            "Polymarket user stream reported confirmed untracked trade {}",
            trade.id
        )));
    }
    let mut events = Vec::new();
    let mut fill_updates = Vec::new();
    for (order_id, venue_id, price, quantity, fee) in candidates {
        let fill_id = format!("{}:{venue_id}", trade.id);
        fill_updates.push(ConfirmedFillUpdate {
            fill_id: fill_id.clone(),
            venue_id,
            quantity,
            timestamp,
        });
        events.push(ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price(price),
            quantity: Quantity(quantity),
            timestamp,
            fill_id: fill_id.clone(),
        });
        if let Some(amount) = fee {
            events.push(ExecutionEvent::FeeCharged {
                order_id,
                amount,
                timestamp,
                fill_id,
            });
        }
    }
    let mut seen = seen_fills.lock().await;
    let mut tracking = tracked.write().await;
    let mut prospective = tracking.clone();
    let mut pending_fill_ids = HashSet::new();
    for update in &fill_updates {
        if !seen.contains(&update.fill_id) && pending_fill_ids.insert(update.fill_id.as_str()) {
            prospective.apply_confirmed_fill(
                &update.venue_id,
                update.quantity,
                update.timestamp,
            )?;
        }
    }
    let events = dedupe_fill_event_groups(events, &mut seen);
    *tracking = prospective;
    Ok(events)
}

fn confirm_cancel(response: &CancelOrdersResponse, venue_id: &str) -> HftResult<()> {
    if response.canceled.iter().any(|id| id == venue_id) {
        return Ok(());
    }
    if let Some(reason) = response.not_canceled.get(venue_id) {
        return Err(HftError::Exchange(format!(
            "Polymarket did not cancel {venue_id}: {reason}"
        )));
    }
    Err(HftError::Execution(format!(
        "Polymarket did not confirm cancellation for {venue_id}"
    )))
}

fn validate_replacement_cancel_state(order: &OpenOrderResponse) -> HftResult<()> {
    if !matches!(order.status, OrderStatusType::Canceled) {
        return Err(HftError::Execution(format!(
            "Polymarket replacement requires a terminal CANCELED order; {} is {}",
            order.id, order.status
        )));
    }
    if order.size_matched != Decimal::ZERO {
        return Err(HftError::Execution(format!(
            "Polymarket order {} matched {} shares during cancellation",
            order.id, order.size_matched
        )));
    }
    Ok(())
}

impl PolymarketExecutionClient {
    async fn authenticate(&self) -> HftResult<AuthenticatedClient> {
        let unauthenticated = ClobClient::new(
            &self.config.host,
            ClobConfig::builder()
                .use_server_time(self.config.use_server_time)
                .heartbeat_interval(Duration::from_secs(5))
                .build(),
        )
        .map_err(|error| HftError::Config(format!("Polymarket CLOB client: {error}")))?;
        let version = unauthenticated.version().await.map_err(map_sdk_error)?;
        if version != 2 {
            return Err(HftError::Config(format!(
                "Polymarket CLOB API v2 is required; host reported v{version}"
            )));
        }
        let geoblock = unauthenticated
            .check_geoblock()
            .await
            .map_err(map_sdk_error)?;
        if geoblock.blocked {
            return Err(HftError::Risk(format!(
                "Polymarket trading is geoblocked from country={} region={}",
                geoblock.country, geoblock.region
            )));
        }
        let mut auth = unauthenticated
            .authentication_builder(&self.signer)
            .signature_type(self.config.signature_type.sdk());
        if self.config.signature_type != WalletSignatureType::Eoa {
            auth = auth.funder(self.principal);
        }
        auth.authenticate().await.map_err(|error| {
            HftError::Authentication(format!("authenticate Polymarket CLOB: {error}"))
        })
    }

    async fn ensure_account_readiness(&self, client: &AuthenticatedClient) -> HftResult<()> {
        validate_account_trade_readiness(client, &self.data_client, self.config.minimum_collateral)
            .await
    }

    async fn start_private_stream(&mut self, client: AuthenticatedClient) -> HftResult<()> {
        let ws = WsClient::new(&self.config.ws_url, WsConfig::default())
            .map_err(|error| HftError::Config(format!("Polymarket user WebSocket: {error}")))?
            .authenticate(client.credentials().clone(), client.address())
            .map_err(|error| {
                HftError::Authentication(format!("Polymarket user WebSocket auth: {error}"))
            })?;
        let stream = ws.subscribe_user_events(Vec::new()).map_err(|error| {
            HftError::Network(format!("Polymarket user WebSocket subscribe: {error}"))
        })?;
        let mut stream: Pin<
            Box<dyn Stream<Item = polymarket_client_sdk::Result<WsMessage>> + Send>,
        > = Box::pin(stream);
        let connected = tokio::time::timeout(USER_WS_CONNECT_TIMEOUT, async {
            loop {
                if ws.connection_state(ChannelType::User).is_connected() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        if connected.is_err() {
            return Err(HftError::Timeout(
                "Polymarket user WebSocket did not connect".to_string(),
            ));
        }
        let tracking = self.tracked.read().await.clone();
        let initial_check_complete = self.initial_account_check_complete.load(Ordering::Acquire);
        let recovery_required = self.account_recovery_required.load(Ordering::Acquire);
        let recovery_preflight =
            if may_bootstrap_account_recovery(&tracking, initial_check_complete, recovery_required)
            {
                Some(load_all_orders(&client).await?)
            } else {
                None
            };
        if recovery_preflight
            .as_ref()
            .is_some_and(|orders| !orders.is_empty())
        {
            let order_count = recovery_preflight.as_ref().map_or(0, Vec::len);
            if recovery_preflight
                .as_deref()
                .is_some_and(recovery_orders_have_unaccounted_fill)
            {
                self.account_recovery_unaccounted_fill
                    .store(true, Ordering::Release);
            }
            self.account_recovery_required
                .store(true, Ordering::Release);
            self.initial_account_check_complete
                .store(true, Ordering::Release);
            self.private_healthy.store(false, Ordering::Release);
            emit_event(
                &self.event_tx,
                &self.pending_events,
                &self.private_healthy,
                ExecutionEvent::ReconciliationRequired {
                    reason: format!(
                        "Polymarket startup found {order_count} exchange-only open order(s); operator recovery is required and new-order intake remains disabled"
                    ),
                    timestamp: now_micros(),
                },
            )
            .await;
        } else {
            for event in rest_catch_up_and_confirm_recovery(
                &client,
                AccountRecoveryContext {
                    data_client: &self.data_client,
                    minimum_collateral: self.config.minimum_collateral,
                    tracked: &self.tracked,
                    seen_fills: &self.seen_fills,
                    catch_up_after: &self.catch_up_after,
                    required: &self.account_recovery_required,
                    unaccounted_fill: &self.account_recovery_unaccounted_fill,
                },
            )
            .await?
            {
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    event,
                )
                .await;
            }
            self.initial_account_check_complete
                .store(true, Ordering::Release);
            self.private_healthy.store(true, Ordering::Release);
        }
        self.last_heartbeat.store(now_micros(), Ordering::Release);

        let tracked = Arc::clone(&self.tracked);
        let seen_fills = Arc::clone(&self.seen_fills);
        let event_tx = self.event_tx.clone();
        let pending_events = Arc::clone(&self.pending_events);
        let private_healthy = Arc::clone(&self.private_healthy);
        let account_recovery_required = Arc::clone(&self.account_recovery_required);
        let account_recovery_unaccounted_fill = Arc::clone(&self.account_recovery_unaccounted_fill);
        let data_client = self.data_client.clone();
        let minimum_collateral = self.config.minimum_collateral;
        let submission_outcome_unknown = Arc::clone(&self.submission_outcome_unknown);
        let last_heartbeat = Arc::clone(&self.last_heartbeat);
        let catch_up_after = Arc::clone(&self.catch_up_after);
        self.private_task = Some(tokio::spawn(async move {
            let mut health = tokio::time::interval_at(
                tokio::time::Instant::now() + USER_WS_HEALTH_INTERVAL,
                USER_WS_HEALTH_INTERVAL,
            );
            health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    item = stream.next() => match item {
                        Some(Ok(WsMessage::Order(order))) => {
                            last_heartbeat.store(now_micros(), Ordering::Release);
                            let reports_fill = order
                                .size_matched
                                .is_some_and(|quantity| quantity > Decimal::ZERO);
                            match private_order_events(order, &tracked).await {
                                Ok(events) => {
                                    for event in events {
                                        emit_event(&event_tx, &pending_events, &private_healthy, event).await;
                                    }
                                }
                                Err(error) => {
                                    if reports_fill
                                        && account_recovery_required.load(Ordering::Acquire)
                                    {
                                        account_recovery_unaccounted_fill
                                            .store(true, Ordering::Release);
                                    }
                                    private_healthy.store(false, Ordering::Release);
                                    emit_event(
                                        &event_tx,
                                        &pending_events,
                                        &private_healthy,
                                        ExecutionEvent::ReconciliationRequired {
                                            reason: error.to_string(),
                                            timestamp: now_micros(),
                                        },
                                    ).await;
                                }
                            }
                        }
                        Some(Ok(WsMessage::Trade(trade))) => {
                            last_heartbeat.store(now_micros(), Ordering::Release);
                            let may_have_unaccounted_fill =
                                private_trade_may_have_unaccounted_fill(&trade.status);
                            match private_trade_events(trade, &tracked, &seen_fills).await {
                                Ok(events) => {
                                    for event in events {
                                        emit_event(&event_tx, &pending_events, &private_healthy, event).await;
                                    }
                                }
                                Err(error) => {
                                    if may_have_unaccounted_fill
                                        && account_recovery_required.load(Ordering::Acquire)
                                    {
                                        account_recovery_unaccounted_fill
                                            .store(true, Ordering::Release);
                                    }
                                    private_healthy.store(false, Ordering::Release);
                                    emit_event(
                                        &event_tx,
                                        &pending_events,
                                        &private_healthy,
                                        ExecutionEvent::ReconciliationRequired {
                                            reason: error.to_string(),
                                            timestamp: now_micros(),
                                        },
                                    ).await;
                                }
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            private_healthy.store(false, Ordering::Release);
                            emit_event(
                                &event_tx,
                                &pending_events,
                                &private_healthy,
                                ExecutionEvent::ReconciliationRequired {
                                    reason: format!("Polymarket user WebSocket error: {error}"),
                                    timestamp: now_micros(),
                                },
                            ).await;
                        }
                        None => {
                            private_healthy.store(false, Ordering::Release);
                            emit_event(
                                &event_tx,
                                &pending_events,
                                &private_healthy,
                                ExecutionEvent::ConnectionStatus {
                                    connected: false,
                                    timestamp: now_micros(),
                                },
                            ).await;
                            break;
                        }
                    },
                    _ = health.tick() => {
                        let ws_connected = ws.connection_state(ChannelType::User).is_connected();
                        if !ws_connected {
                            if private_healthy.swap(false, Ordering::AcqRel) {
                                emit_event(
                                    &event_tx,
                                    &pending_events,
                                    &private_healthy,
                                    ExecutionEvent::ConnectionStatus {
                                        connected: false,
                                        timestamp: now_micros(),
                                    },
                                ).await;
                                emit_event(
                                    &event_tx,
                                    &pending_events,
                                    &private_healthy,
                                    ExecutionEvent::ReconciliationRequired {
                                        reason: "Polymarket user WebSocket disconnected".to_string(),
                                        timestamp: now_micros(),
                                    },
                                ).await;
                            }
                            continue;
                        }
                        if !private_healthy.load(Ordering::Acquire) {
                            if submission_outcome_unknown.load(Ordering::Acquire) {
                                continue;
                            }
                            match rest_catch_up_and_confirm_recovery(
                                &client,
                                AccountRecoveryContext {
                                    data_client: &data_client,
                                    minimum_collateral,
                                    tracked: &tracked,
                                    seen_fills: &seen_fills,
                                    catch_up_after: &catch_up_after,
                                    required: &account_recovery_required,
                                    unaccounted_fill: &account_recovery_unaccounted_fill,
                                },
                            )
                            .await
                            {
                                Ok(events) => {
                                    for event in events {
                                        emit_event(&event_tx, &pending_events, &private_healthy, event).await;
                                    }
                                    private_healthy.store(true, Ordering::Release);
                                    last_heartbeat.store(now_micros(), Ordering::Release);
                                    emit_event(
                                        &event_tx,
                                        &pending_events,
                                        &private_healthy,
                                        ExecutionEvent::ConnectionStatus {
                                            connected: true,
                                            timestamp: now_micros(),
                                        },
                                    ).await;
                                }
                                Err(error) => {
                                    emit_event(
                                        &event_tx,
                                        &pending_events,
                                        &private_healthy,
                                        ExecutionEvent::ReconciliationRequired {
                                            reason: format!("Polymarket REST catch-up failed: {error}"),
                                            timestamp: now_micros(),
                                        },
                                    ).await;
                                }
                            }
                        } else {
                            last_heartbeat.store(now_micros(), Ordering::Release);
                        }
                    }
                }
            }
        }));
        Ok(())
    }
}

#[async_trait]
impl ExecutionClient for PolymarketExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let envelope = OrderIntentEnvelope::new(intent, Default::default());
        self.place_order_envelope(&envelope).await
    }

    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        let logical_id = self.submit_envelope(envelope, None).await?;
        emit_event(
            &self.event_tx,
            &self.pending_events,
            &self.private_healthy,
            ExecutionEvent::OrderAck {
                order_id: logical_id.clone(),
                timestamp: now_micros(),
            },
        )
        .await;
        Ok(logical_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        // Cancellation is risk-reducing and remains available through authenticated REST while
        // private/account recovery keeps all new exposure disabled.
        let client = self.ensure_cancel_ready()?;
        let venue_id = self.resolve_venue_id(order_id).await;
        let response = match client.cancel_order(&venue_id).await {
            Ok(response) => response,
            Err(error) => {
                let error = map_sdk_error(error);
                if matches!(error, HftError::Network(_) | HftError::Timeout(_)) {
                    self.require_reconciliation(
                        format!(
                            "Polymarket cancellation outcome is unknown for order {venue_id}: {error}"
                        ),
                        false,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        confirm_cancel(&response, &venue_id)?;
        self.tracked.write().await.terminalize(order_id);
        emit_event(
            &self.event_tx,
            &self.pending_events,
            &self.private_healthy,
            ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: now_micros(),
            },
        )
        .await;
        Ok(())
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        if new_quantity.is_none() && new_price.is_none() {
            return Err(HftError::InvalidOrder(
                "Polymarket replacement requires quantity or price".to_string(),
            ));
        }
        let venue_id = self.resolve_venue_id(order_id).await;
        let existing = self
            .ensure_ready()?
            .order(&venue_id)
            .await
            .map_err(map_sdk_error)?;
        let remaining = (existing.original_size - existing.size_matched).max(Decimal::ZERO);
        if remaining <= Decimal::ZERO {
            return Err(HftError::OrderNotFound(format!(
                "Polymarket order {venue_id} has no remaining quantity"
            )));
        }
        if existing.size_matched > Decimal::ZERO {
            return Err(HftError::InvalidOrder(format!(
                "Polymarket order {venue_id} is partially filled; cancel it and submit a new reviewed intent after reconciliation"
            )));
        }
        let tracked = self
            .tracked
            .read()
            .await
            .active_order(order_id)
            .cloned()
            .ok_or_else(|| {
                HftError::OrderNotFound(format!(
                    "Polymarket order {order_id:?} has no tracked signed execution policy"
                ))
            })?;
        let quantity = new_quantity.unwrap_or(Quantity(remaining));
        let price = new_price.unwrap_or(Price(existing.price));
        if quantity.0 > remaining {
            return Err(HftError::Risk(format!(
                "Polymarket replacement cannot increase quantity from {remaining} to {} without a new risk-reviewed intent",
                quantity.0
            )));
        }
        let existing_side = hft_side(existing.side)?;
        let price_increases_risk = match existing_side {
            Side::Buy => price.0 > existing.price,
            Side::Sell => price.0 < existing.price,
        };
        if price_increases_risk {
            return Err(HftError::Risk(format!(
                "Polymarket replacement price {} is more aggressive than {}; cancel and submit a new risk-reviewed intent",
                price.0, existing.price
            )));
        }
        let intent = OrderIntent {
            symbol: Symbol::new(existing.asset_id.to_string()),
            asset_class: AssetClass::PredictionMarket,
            product_type: ProductType::PredictionMarket,
            compliance_context: Default::default(),
            side: existing_side,
            quantity,
            order_type: HftOrderType::Limit,
            price: Some(price),
            time_in_force: TimeInForce::GTC,
            strategy_id: tracked.intent.strategy_id.clone(),
            target_venue: Some(VenueId::POLYMARKET),
        };
        self.replacement_sequence = self.replacement_sequence.saturating_add(1);
        let replacement_client_id = format!(
            "{}-r{}",
            tracked
                .client_order_id
                .as_deref()
                .unwrap_or(order_id.0.as_str()),
            self.replacement_sequence
        );
        let envelope = OrderIntentEnvelope::new(intent.clone(), tracked.lifecycle)
            .with_client_order_id(replacement_client_id);
        self.prepare_order(self.ensure_ready()?, &envelope).await?;

        let cancel = match self.ensure_ready()?.cancel_order(&venue_id).await {
            Ok(cancel) => cancel,
            Err(error) => {
                let error = map_sdk_error(error);
                if matches!(error, HftError::Network(_) | HftError::Timeout(_)) {
                    self.require_reconciliation(
                        format!(
                            "Polymarket replacement cancellation outcome is unknown for order {venue_id}: {error}"
                        ),
                        false,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        confirm_cancel(&cancel, &venue_id)?;
        // Keep the old venue alias before submitting the replacement. A CONFIRMED fill may arrive
        // after the cancellation response, including while the new submit is in flight.
        self.tracked.write().await.terminalize(order_id);
        let canceled_order = match self.ensure_ready()?.order(&venue_id).await {
            Ok(order) => order,
            Err(error) => {
                let error = map_sdk_error(error);
                self.require_reconciliation(
                    format!(
                        "Polymarket replacement could not verify canceled order {venue_id}: {error}"
                    ),
                    false,
                )
                .await;
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: now_micros(),
                    },
                )
                .await;
                return Err(error);
            }
        };
        let canceled_side = match hft_side(canceled_order.side) {
            Ok(side) => side,
            Err(error) => {
                self.require_reconciliation(error.to_string(), false).await;
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: now_micros(),
                    },
                )
                .await;
                return Err(error);
            }
        };
        if let Err(error) = validate_tracked_identity(
            &tracked,
            canceled_order.asset_id,
            canceled_side,
            "replacement post-cancel check",
        ) {
            self.require_reconciliation(error.to_string(), false).await;
            emit_event(
                &self.event_tx,
                &self.pending_events,
                &self.private_healthy,
                ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: now_micros(),
                },
            )
            .await;
            return Err(error);
        }
        if let Err(error) = validate_replacement_cancel_state(&canceled_order) {
            self.require_reconciliation(error.to_string(), false).await;
            emit_event(
                &self.event_tx,
                &self.pending_events,
                &self.private_healthy,
                ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: now_micros(),
                },
            )
            .await;
            return Err(error);
        }
        match self.submit_envelope(&envelope, Some(order_id)).await {
            Ok(_) => {
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    ExecutionEvent::OrderModified {
                        order_id: order_id.clone(),
                        new_quantity,
                        new_price,
                        timestamp: now_micros(),
                    },
                )
                .await;
                Ok(())
            }
            Err(error @ HftError::Network(_)) | Err(error @ HftError::Timeout(_)) => Err(error),
            Err(error) => {
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: now_micros(),
                    },
                )
                .await;
                Err(HftError::Execution(format!(
                    "Polymarket replacement canceled the original; new order failed: {error}"
                )))
            }
        }
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let receiver = self.event_tx.subscribe();
        let pending = {
            let mut pending = self.pending_events.lock().await;
            std::mem::take(&mut *pending)
        };
        let connected =
            self.connected.load(Ordering::Acquire) && self.private_healthy.load(Ordering::Acquire);
        let initial = stream::once(async move {
            Ok(ExecutionEvent::ConnectionStatus {
                connected,
                timestamp: now_micros(),
            })
        });
        let pending = stream::iter(pending.into_iter().map(Ok));
        let private_healthy = Arc::clone(&self.private_healthy);
        let events = BroadcastStream::new(receiver).filter_map(move |result| {
            let private_healthy = Arc::clone(&private_healthy);
            async move {
                match result {
                    Ok(event) => Some(Ok(event)),
                    Err(error) => {
                        private_healthy.store(false, Ordering::Release);
                        Some(Ok(ExecutionEvent::ReconciliationRequired {
                            reason: format!("Polymarket private event stream lagged: {error}"),
                            timestamp: now_micros(),
                        }))
                    }
                }
            }
        });
        Ok(Box::pin(initial.chain(pending).chain(events)))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        let client = self.authenticated()?;
        let tracked = self.tracked.read().await.clone();
        let aliases = tracked.aliases_by_venue();
        load_all_orders(client)
            .await?
            .into_iter()
            .map(|order| open_order(order, &aliases))
            .filter_map(|order| order.transpose())
            .collect()
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        let client = self.authenticated()?;
        let collateral = client
            .balance_allowance(
                BalanceAllowanceRequest::builder()
                    .asset_type(AssetType::Collateral)
                    .build(),
            )
            .await
            .map_err(map_sdk_error)?;
        let cash = raw_balance_to_units(collateral.balance)?;
        let mut balances = vec![AccountBalance {
            asset: "USDC".to_string(),
            available: cash,
            frozen: Decimal::ZERO,
            total: cash,
            usd_value: Some(cash),
        }];
        balances.extend(
            self.load_positions()
                .await?
                .into_iter()
                .map(|position| AccountBalance {
                    asset: format!("OUTCOME:{}", position.asset),
                    available: position.size,
                    frozen: Decimal::ZERO,
                    total: position.size,
                    usd_value: Some(position.current_value),
                }),
        );
        Ok(balances)
    }

    fn supports_position_snapshot(&self) -> bool {
        true
    }

    async fn get_positions(&self) -> HftResult<Vec<Position>> {
        Ok(self
            .load_positions()
            .await?
            .into_iter()
            .map(|position| Position {
                symbol: Symbol::new(position.asset.to_string()),
                quantity: Quantity(position.size),
                avg_price: Price(position.avg_price),
                unrealized_pnl: position.cash_pnl,
            })
            .collect())
    }

    fn supports_recent_fills_snapshot(&self) -> bool {
        true
    }

    async fn list_recent_fills(&self) -> HftResult<Vec<AccountFill>> {
        let client = self.authenticated()?;
        let tracked = self.tracked.read().await.clone();
        let aliases = tracked.aliases_by_venue();
        let api_key = client.credentials().key();
        let mut fills = Vec::new();
        let mut ids = HashSet::new();
        for trade in load_all_trades(client, &TradesRequest::default()).await? {
            for fill in account_fills_from_trade(&trade, api_key, &aliases)? {
                if ids.insert(fill.fill_id.clone()) {
                    fills.push(fill);
                }
            }
        }
        Ok(fills)
    }

    async fn connect(&mut self) -> HftResult<()> {
        if self.submission_outcome_unknown.load(Ordering::Acquire) {
            return Err(HftError::Execution(
                "Polymarket submission outcome remains unknown; inspect the account and restart the process"
                    .to_string(),
            ));
        }
        if self.connected.load(Ordering::Acquire)
            && (self.private_healthy.load(Ordering::Acquire)
                || (self.account_recovery_required.load(Ordering::Acquire)
                    && self.private_stream_running()))
        {
            return Ok(());
        }
        if let Some(task) = self.private_task.take() {
            task.abort();
        }
        self.connected.store(false, Ordering::Release);
        self.private_healthy.store(false, Ordering::Release);
        let client = self.authenticate().await?;
        let tracking = self.tracked.read().await.clone();
        let recovery_preflight = if may_bootstrap_account_recovery(
            &tracking,
            self.initial_account_check_complete.load(Ordering::Acquire),
            self.account_recovery_required.load(Ordering::Acquire),
        ) {
            load_all_orders(&client).await?
        } else {
            Vec::new()
        };
        if !recovery_preflight.is_empty() {
            if recovery_orders_have_unaccounted_fill(&recovery_preflight) {
                self.account_recovery_unaccounted_fill
                    .store(true, Ordering::Release);
            }
            self.account_recovery_required
                .store(true, Ordering::Release);
            self.initial_account_check_complete
                .store(true, Ordering::Release);
        }
        if !self.account_recovery_required.load(Ordering::Acquire) {
            self.ensure_account_readiness(&client).await?;
        }

        // Authenticated REST is the recovery control plane. Publish it before private-stream
        // startup so an existing exchange-only order remains cancellable even if WS startup fails.
        self.client = Some(client.clone());
        self.connected.store(true, Ordering::Release);
        if let Err(error) = self.start_private_stream(client).await {
            if self.account_recovery_required.load(Ordering::Acquire) {
                emit_event(
                    &self.event_tx,
                    &self.pending_events,
                    &self.private_healthy,
                    ExecutionEvent::ReconciliationRequired {
                        reason: format!(
                            "Polymarket private stream unavailable during account recovery; authenticated REST cancellation remains available: {error}"
                        ),
                        timestamp: now_micros(),
                    },
                )
                .await;
            } else {
                self.client = None;
                self.connected.store(false, Ordering::Release);
                return Err(error);
            }
        }
        let execution_ready = self.execution_ready();
        emit_event(
            &self.event_tx,
            &self.pending_events,
            &self.private_healthy,
            ExecutionEvent::ConnectionStatus {
                connected: execution_ready,
                timestamp: now_micros(),
            },
        )
        .await;
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(task) = self.private_task.take() {
            task.abort();
        }
        self.client = None;
        self.connected.store(false, Ordering::Release);
        self.private_healthy.store(false, Ordering::Release);
        emit_event(
            &self.event_tx,
            &self.pending_events,
            &self.private_healthy,
            ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: now_micros(),
            },
        )
        .await;
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.execution_ready(),
            latency_ms: None,
            last_heartbeat: self.last_heartbeat.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_test_client() -> PolymarketExecutionClient {
        PolymarketExecutionClient::new(PolymarketExecutionConfig {
            private_key: Some(SecretString::from(format!("0x{:064x}", 1))),
            ..Default::default()
        })
        .expect("valid local execution gate fixture")
    }

    fn intent(token: &str) -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new(token),
            asset_class: AssetClass::PredictionMarket,
            product_type: ProductType::PredictionMarket,
            compliance_context: Default::default(),
            side: Side::Buy,
            quantity: Quantity(Decimal::from(5)),
            order_type: HftOrderType::Limit,
            price: Some(Price(Decimal::new(5, 1))),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: Some(VenueId::POLYMARKET),
        }
    }

    fn tracked_orders() -> Arc<RwLock<TrackingBook>> {
        Arc::new(RwLock::new(TrackingBook {
            active: HashMap::from([(
                "logical-1".to_string(),
                tracked_order("logical-1", "venue-1", "client-1"),
            )]),
            terminal_by_venue: HashMap::new(),
        }))
    }

    fn tracked_order(logical_id: &str, venue_id: &str, client_order_id: &str) -> TrackedOrder {
        TrackedOrder {
            logical_id: OrderId(logical_id.to_string()),
            venue_id: venue_id.to_string(),
            client_order_id: Some(client_order_id.to_string()),
            intent: intent("123"),
            lifecycle: OrderIntentLifecycle {
                max_slippage_bps: Some(100),
                max_order_notional: Some(Decimal::from(10)),
                ..Default::default()
            },
            created_at: 1,
            remaining_quantity: Decimal::from(5),
        }
    }

    fn api_key() -> polymarket_client_sdk::auth::ApiKey {
        "ffffffff-ffff-ffff-ffff-ffffffffffff"
            .parse()
            .expect("API key fixture")
    }

    fn rest_open_order(venue_id: &str) -> OpenOrderResponse {
        serde_json::from_value(serde_json::json!({
            "id": venue_id,
            "status": "LIVE",
            "owner": api_key(),
            "maker_address": "0x2222222222222222222222222222222222222222",
            "market": B256::ZERO.to_string(),
            "asset_id": "123",
            "side": "BUY",
            "original_size": "5",
            "size_matched": "0",
            "price": "0.5",
            "associate_trades": [],
            "outcome": "YES",
            "created_at": 1_705_322_096,
            "expiration": "1705708800",
            "order_type": "GTC"
        }))
        .expect("REST order fixture")
    }

    fn rest_trade(venue_id: &str) -> TradeResponse {
        serde_json::from_value(serde_json::json!({
            "id": "trade-rest-1",
            "taker_order_id": venue_id,
            "market": B256::ZERO.to_string(),
            "asset_id": "123",
            "side": "BUY",
            "size": "5",
            "fee_rate_bps": "100",
            "price": "0.5",
            "status": "CONFIRMED",
            "match_time": "1705322096",
            "last_update": "1705322130",
            "outcome": "YES",
            "bucket_index": 0,
            "owner": api_key(),
            "maker_address": "0x2222222222222222222222222222222222222222",
            "maker_orders": [],
            "transaction_hash": B256::ZERO.to_string(),
            "trader_side": "TAKER"
        }))
        .expect("REST trade fixture")
    }

    fn private_order(status: &str) -> OrderMessage {
        private_order_with_progress(status, "5")
    }

    fn private_order_with_progress(status: &str, size_matched: &str) -> OrderMessage {
        serde_json::from_value(serde_json::json!({
            "asset_id": "123",
            "event_type": "order",
            "id": "venue-1",
            "market": B256::ZERO.to_string(),
            "original_size": "5",
            "price": "0.5",
            "side": "BUY",
            "size_matched": size_matched,
            "status": status,
            "timestamp": "1000",
            "type": "UPDATE"
        }))
        .expect("private order fixture")
    }

    fn private_trade(status: &str, asset_id: &str, venue_id: &str) -> TradeMessage {
        serde_json::from_value(serde_json::json!({
            "asset_id": asset_id,
            "event_type": "trade",
            "id": "trade-1",
            "maker_orders": [],
            "market": B256::ZERO.to_string(),
            "matchtime": "1000",
            "price": "0.5",
            "side": "BUY",
            "size": "5",
            "fee_rate_bps": "100",
            "status": status,
            "taker_order_id": venue_id,
            "timestamp": "1000",
            "type": "TRADE"
        }))
        .expect("private trade fixture")
    }

    fn private_trade_without_fee(venue_id: &str) -> TradeMessage {
        serde_json::from_value(serde_json::json!({
            "asset_id": "123",
            "event_type": "trade",
            "id": "trade-no-fee",
            "maker_orders": [],
            "market": B256::ZERO.to_string(),
            "matchtime": "1000",
            "price": "0.5",
            "side": "BUY",
            "size": "5",
            "status": "CONFIRMED",
            "taker_order_id": venue_id,
            "timestamp": "1000",
            "trader_side": "TAKER",
            "type": "TRADE"
        }))
        .expect("private taker trade without fee fixture")
    }

    fn private_maker_trade() -> TradeMessage {
        serde_json::from_value(serde_json::json!({
            "asset_id": "999",
            "event_type": "trade",
            "id": "trade-maker-1",
            "maker_orders": [{
                "asset_id": "123",
                "matched_amount": "5",
                "order_id": "venue-1",
                "outcome": "YES",
                "owner": api_key(),
                "price": "0.5"
            }],
            "market": B256::ZERO.to_string(),
            "matchtime": "1000",
            "price": "0.5",
            "side": "SELL",
            "size": "5",
            "status": "CONFIRMED",
            "taker_order_id": "external-taker",
            "timestamp": "1000",
            "trader_side": "MAKER",
            "type": "TRADE"
        }))
        .expect("private maker trade fixture")
    }

    fn rest_maker_trade() -> TradeResponse {
        serde_json::from_value(serde_json::json!({
            "id": "trade-rest-maker-1",
            "taker_order_id": "external-taker",
            "market": B256::ZERO.to_string(),
            "asset_id": "999",
            "side": "SELL",
            "size": "5",
            "fee_rate_bps": "-1",
            "price": "0.5",
            "status": "CONFIRMED",
            "match_time": "1705322096",
            "last_update": "1705322130",
            "outcome": "NO",
            "bucket_index": 0,
            "owner": api_key(),
            "maker_address": "0x2222222222222222222222222222222222222222",
            "maker_orders": [{
                "order_id": "venue-1",
                "owner": api_key(),
                "maker_address": "0x2222222222222222222222222222222222222222",
                "matched_amount": "5",
                "price": "0.5",
                "fee_rate_bps": "0",
                "asset_id": "123",
                "outcome": "YES",
                "side": "BUY"
            }],
            "transaction_hash": B256::ZERO.to_string(),
            "trader_side": "MAKER"
        }))
        .expect("REST maker trade fixture")
    }

    #[test]
    fn validates_identity_slippage_and_cancel_confirmation_without_secrets() {
        assert!(PolymarketExecutionClient::new(PolymarketExecutionConfig::default()).is_err());
        assert_eq!(
            WalletSignatureType::from_str("gnosis-safe").unwrap(),
            WalletSignatureType::GnosisSafe
        );
        assert!(parse_token_id(&Symbol::new("00123")).is_err());
        assert_eq!(
            slippage_price(Decimal::new(50, 2), Side::Buy, 100, Decimal::new(1, 2)).unwrap(),
            Decimal::new(50, 2)
        );
        assert_eq!(
            slippage_price(Decimal::new(50, 2), Side::Sell, 100, Decimal::new(1, 2)).unwrap(),
            Decimal::new(50, 2)
        );
        let cancel = CancelOrdersResponse::builder()
            .canceled(vec!["venue-1".to_string()])
            .build();
        assert!(confirm_cancel(&cancel, "venue-1").is_ok());
        assert!(confirm_cancel(&cancel, "venue-2").is_err());
        assert_eq!(envelope_metadata("monday-1"), envelope_metadata("monday-1"));
        assert_eq!(
            conditional_balance_to_shares(Decimal::from(5_000_000)).unwrap(),
            Decimal::from(5)
        );
        assert_eq!(
            conditional_balance_to_shares(Decimal::from_str("4.881280").unwrap()).unwrap(),
            Decimal::from_str("4.881280").unwrap()
        );
        assert_eq!(
            slippage_price(Decimal::new(501, 3), Side::Buy, 100, Decimal::new(25, 4)).unwrap(),
            Decimal::new(505, 3)
        );
    }

    #[test]
    fn pristine_exchange_only_startup_enters_recovery_without_enabling_exposure() {
        let tracking = TrackingBook::default();
        let exchange_orders = [rest_open_order("legacy-venue-order")];
        assert!(may_bootstrap_account_recovery(&tracking, false, false));
        assert!(!exchange_orders.is_empty());

        let client = gate_test_client();
        client.connected.store(true, Ordering::Release);
        client.private_healthy.store(false, Ordering::Release);
        client
            .account_recovery_required
            .store(true, Ordering::Release);

        assert!(!client.execution_ready());
        assert!(matches!(client.ensure_ready(), Err(HftError::Risk(_))));
        assert!(
            client.ensure_cancel_transport_ready().is_ok(),
            "authenticated REST cancellation remains a permitted risk-reducing path"
        );

        let mut later_tracking = TrackingBook::default();
        later_tracking.activate(tracked_order("logical-1", "venue-1", "client-1"));
        assert!(
            !may_bootstrap_account_recovery(&later_tracking, true, false),
            "unknown later activity must not reuse the pristine-startup exception"
        );

        let recovery_latch = AtomicBool::new(true);
        let unaccounted_fill = AtomicBool::new(false);
        assert!(confirm_account_recovery_open_orders(
            &exchange_orders,
            &recovery_latch,
            &unaccounted_fill,
        )
        .is_err());
        assert!(recovery_latch.load(Ordering::Acquire));
        confirm_account_recovery_open_orders(&[], &recovery_latch, &unaccounted_fill)
            .expect("a final complete empty open-order snapshot clears the recovery latch");
        assert!(!recovery_latch.load(Ordering::Acquire));

        let mut partially_filled = rest_open_order("partially-filled-legacy-order");
        partially_filled.size_matched = Decimal::ONE;
        assert!(recovery_orders_have_unaccounted_fill(&[partially_filled]));
        recovery_latch.store(true, Ordering::Release);
        unaccounted_fill.store(true, Ordering::Release);
        assert!(
            confirm_account_recovery_open_orders(&[], &recovery_latch, &unaccounted_fill,).is_err()
        );
        assert!(
            recovery_latch.load(Ordering::Acquire),
            "pre-start partial fills require explicit portfolio reconciliation"
        );
    }

    #[tokio::test]
    async fn finished_private_task_is_not_treated_as_a_live_recovery_stream() {
        let mut client = gate_test_client();
        let finished = tokio::spawn(async {});
        while !finished.is_finished() {
            tokio::task::yield_now().await;
        }
        client.private_task = Some(finished);
        assert!(!client.private_stream_running());

        let running = tokio::spawn(std::future::pending::<()>());
        client.private_task = Some(running);
        assert!(client.private_stream_running());
        client.private_task.take().expect("running task").abort();
    }

    #[test]
    fn replacement_requires_authoritative_zero_fill_canceled_state() {
        let mut canceled = rest_open_order("venue-1");
        canceled.status = OrderStatusType::Canceled;
        assert!(validate_replacement_cancel_state(&canceled).is_ok());

        canceled.size_matched = Decimal::new(1, 1);
        assert!(validate_replacement_cancel_state(&canceled).is_err());

        canceled.size_matched = Decimal::ZERO;
        canceled.status = OrderStatusType::Live;
        assert!(validate_replacement_cancel_state(&canceled).is_err());
    }

    #[test]
    fn all_supported_order_types_enforce_signed_midpoint_slippage_boundaries() {
        let midpoint = Decimal::new(5, 1);
        let tick = Decimal::new(1, 3);
        let bps = required_max_slippage_bps(Some(100)).unwrap();

        for time_in_force in [TimeInForce::GTC, TimeInForce::IOC, TimeInForce::FOK] {
            assert!(execution_price_policy(
                HftOrderType::Limit,
                time_in_force,
                Some(Price(Decimal::new(505, 3))),
                Side::Buy,
                midpoint,
                bps,
                tick,
            )
            .is_ok());
            assert!(execution_price_policy(
                HftOrderType::Limit,
                time_in_force,
                Some(Price(Decimal::new(506, 3))),
                Side::Buy,
                midpoint,
                bps,
                tick,
            )
            .is_err());
            assert!(execution_price_policy(
                HftOrderType::Limit,
                time_in_force,
                Some(Price(Decimal::new(495, 3))),
                Side::Sell,
                midpoint,
                bps,
                tick,
            )
            .is_ok());
            assert!(execution_price_policy(
                HftOrderType::Limit,
                time_in_force,
                Some(Price(Decimal::new(494, 3))),
                Side::Sell,
                midpoint,
                bps,
                tick,
            )
            .is_err());
        }

        for time_in_force in [TimeInForce::IOC, TimeInForce::FOK] {
            let (price, _, immediate) = execution_price_policy(
                HftOrderType::Market,
                time_in_force,
                Some(Price(Decimal::new(49, 2))),
                Side::Buy,
                midpoint,
                bps,
                tick,
            )
            .unwrap();
            assert_eq!(price, Decimal::new(505, 3));
            assert!(immediate);
        }
        assert!(execution_price_policy(
            HftOrderType::Market,
            TimeInForce::GTC,
            Some(Price(midpoint)),
            Side::Buy,
            midpoint,
            bps,
            tick,
        )
        .is_err());
        assert!(execution_price_policy(
            HftOrderType::Limit,
            TimeInForce::GTC,
            Some(Price(Decimal::new(5045, 4))),
            Side::Buy,
            midpoint,
            bps,
            tick,
        )
        .is_err());
    }

    #[test]
    fn signed_execution_limits_are_required_and_final_notional_uses_venue_price() {
        assert!(required_max_slippage_bps(None).is_err());
        assert!(required_max_slippage_bps(Some(0)).is_err());
        assert!(required_max_slippage_bps(Some(10_001)).is_err());

        let lifecycle = ports::OrderIntentLifecycle {
            max_slippage_bps: Some(100),
            max_order_notional: Some(Decimal::new(25, 1)),
            ..Default::default()
        };
        let envelope = OrderIntentEnvelope::new(intent("123"), lifecycle);
        assert!(
            validate_final_order_notional(&envelope, Decimal::from(5), Decimal::new(5, 1),).is_ok()
        );
        assert!(
            validate_final_order_notional(&envelope, Decimal::from(5), Decimal::new(501, 3),)
                .is_err()
        );

        let missing_notional = OrderIntentEnvelope::new(
            intent("123"),
            ports::OrderIntentLifecycle {
                max_slippage_bps: Some(100),
                ..Default::default()
            },
        );
        assert!(validate_final_order_notional(
            &missing_notional,
            Decimal::from(5),
            Decimal::new(5, 1),
        )
        .is_err());
    }

    #[tokio::test]
    async fn private_fills_are_confirmed_only_and_identity_checked() {
        let tracked = tracked_orders();
        let seen = Arc::new(Mutex::new(FillDeduper::default()));

        let events = private_trade_events(
            private_trade("CONFIRMED", "123", "venue-1"),
            &tracked,
            &seen,
        )
        .await
        .unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                ExecutionEvent::Fill { order_id, fill_id, .. },
                ExecutionEvent::FeeCharged { fill_id: fee_fill_id, amount, .. }
            ] if order_id.0 == "logical-1"
                && fill_id == fee_fill_id
                && *amount == Decimal::new(125, 4)
        ));
        assert!(tracked.read().await.active.is_empty());
        assert!(tracked
            .read()
            .await
            .terminal_by_venue
            .contains_key("venue-1"));

        assert!(private_trade_events(
            private_trade("CONFIRMED", "124", "venue-1"),
            &tracked,
            &Arc::new(Mutex::new(FillDeduper::default())),
        )
        .await
        .is_err());
        assert!(private_trade_events(
            private_trade("CONFIRMED", "123", "external-order"),
            &tracked,
            &Arc::new(Mutex::new(FillDeduper::default())),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn private_pending_settlement_requires_reconciliation_without_booking_a_fill() {
        for status in ["MATCHED", "MINED", "RETRYING"] {
            let tracked = tracked_orders();
            let seen = Arc::new(Mutex::new(FillDeduper::default()));

            assert!(
                private_trade_events(private_trade(status, "123", "venue-1"), &tracked, &seen)
                    .await
                    .is_err(),
                "{status} must trip the private reconciliation path"
            );
            let tracking = tracked.read().await;
            assert_eq!(
                tracking.active["logical-1"].remaining_quantity,
                Decimal::from(5),
                "a settlement-pending trade is not a confirmed fill"
            );
            assert!(tracking.terminal_by_venue.is_empty());
            assert!(seen.lock().await.ids.is_empty());
        }

        let tracked = tracked_orders();
        let seen = Arc::new(Mutex::new(FillDeduper::default()));
        assert!(
            private_trade_events(private_trade("FAILED", "123", "venue-1"), &tracked, &seen)
                .await
                .is_err(),
            "FAILED must keep private health latched until strict REST reconciliation"
        );
        assert_eq!(
            tracked.read().await.active["logical-1"].remaining_quantity,
            Decimal::from(5)
        );

        let confirmed = private_trade_events(
            private_trade("CONFIRMED", "123", "venue-1"),
            &tracked,
            &seen,
        )
        .await
        .expect("a later CONFIRMED update books the fill through the normal dedupe path");
        assert!(matches!(
            confirmed.as_slice(),
            [
                ExecutionEvent::Fill { .. },
                ExecutionEvent::FeeCharged { .. }
            ]
        ));
        assert!(tracked.read().await.active.is_empty());
    }

    #[tokio::test]
    async fn confirmed_taker_fee_is_required_but_maker_has_no_fee_event() {
        let tracked = tracked_orders();

        assert!(private_trade_events(
            private_trade_without_fee("venue-1"),
            &tracked,
            &Arc::new(Mutex::new(FillDeduper::default())),
        )
        .await
        .is_err());

        let maker_events = private_trade_events(
            private_maker_trade(),
            &tracked,
            &Arc::new(Mutex::new(FillDeduper::default())),
        )
        .await
        .expect("maker fill does not require a taker fee rate");
        assert!(matches!(
            maker_events.as_slice(),
            [ExecutionEvent::Fill { order_id, .. }] if order_id.0 == "logical-1"
        ));
    }

    #[test]
    fn polymarket_taker_fee_uses_five_decimal_rounding_and_validates_bps() {
        assert_eq!(
            polymarket_taker_fee(Decimal::from(10), Decimal::new(5, 1), Decimal::from(100),)
                .unwrap(),
            Decimal::new(25, 3)
        );
        assert_eq!(
            polymarket_taker_fee(Decimal::ONE, Decimal::new(1, 2), Decimal::ONE,).unwrap(),
            Decimal::ZERO
        );
        assert!(
            polymarket_taker_fee(Decimal::ONE, Decimal::new(5, 1), Decimal::NEGATIVE_ONE,).is_err()
        );
        assert!(
            polymarket_taker_fee(Decimal::ONE, Decimal::new(5, 1), Decimal::new(15, 1),).is_err()
        );
    }

    #[test]
    fn rest_taker_fill_and_fee_are_deduped_as_one_group_while_maker_fee_is_zero() {
        let tracking = tracked_orders().blocking_read().clone();
        let aliases = tracking.aliases_by_venue();
        let taker_fills = account_fills_from_trade(&rest_trade("venue-1"), api_key(), &aliases)
            .expect("REST taker fill");
        assert_eq!(taker_fills[0].fee, Some(Decimal::new(125, 4)));

        let maker_fills = account_fills_from_trade(&rest_maker_trade(), api_key(), &aliases)
            .expect("REST maker fill");
        assert_eq!(maker_fills[0].fee, Some(Decimal::ZERO));

        let raw =
            account_activity_events(&[], &[rest_trade("venue-1")], &tracking, api_key(), None)
                .expect("REST catch-up event group")
                .events;
        let mut seen = FillDeduper::default();
        let first = dedupe_fill_event_groups(raw.clone(), &mut seen);
        let second = dedupe_fill_event_groups(raw, &mut seen);
        assert!(matches!(
            first.as_slice(),
            [ExecutionEvent::Fill { fill_id, .. }, ExecutionEvent::FeeCharged { fill_id: fee_fill_id, .. }]
                if fill_id == fee_fill_id
        ));
        assert!(
            second.is_empty(),
            "fill and fee group is deducted only once"
        );
    }

    #[test]
    fn rest_pending_settlement_blocks_recovery_until_a_final_trade_state() {
        let empty_tracking = TrackingBook::default();
        for status in [
            TradeStatusType::Matched,
            TradeStatusType::Mined,
            TradeStatusType::Retrying,
        ] {
            let mut trade = rest_trade("exchange-only-order");
            trade.status = status;
            let recovery_unaccounted_fill = AtomicBool::new(false);

            assert!(
                account_activity_events(
                    &[],
                    &[trade.clone()],
                    &empty_tracking,
                    api_key(),
                    Some(&recovery_unaccounted_fill),
                )
                .is_err(),
                "a settlement-pending REST trade must abort this catch-up"
            );
            assert!(
                recovery_unaccounted_fill.load(Ordering::Acquire),
                "recovery cannot auto-clear while a pre-start trade may still confirm"
            );
            assert!(
                account_fills_from_trade(&trade, api_key(), &HashMap::new()).is_err(),
                "REST fill snapshots must not silently omit settlement-pending activity"
            );
        }

        let recovery_unaccounted_fill = AtomicBool::new(false);
        let mut failed = rest_trade("exchange-only-order");
        failed.status = TradeStatusType::Failed;
        let failed_activity = account_activity_events(
            &[],
            &[failed],
            &empty_tracking,
            api_key(),
            Some(&recovery_unaccounted_fill),
        )
        .expect("FAILED is a final no-fill state after the strict catch-up inspects the account");
        assert!(failed_activity.events.is_empty());
        assert!(failed_activity.fill_updates.is_empty());
        assert!(failed_activity.failed_orders.is_empty());
        assert!(!recovery_unaccounted_fill.load(Ordering::Acquire));

        let tracking = tracked_orders().blocking_read().clone();
        let confirmed =
            account_activity_events(&[], &[rest_trade("venue-1")], &tracking, api_key(), None)
                .expect("CONFIRMED remains the only fill-booking REST state");
        assert!(matches!(
            confirmed.events.as_slice(),
            [
                ExecutionEvent::Fill { .. },
                ExecutionEvent::FeeCharged { .. }
            ]
        ));
        assert_eq!(confirmed.fill_updates.len(), 1);
    }

    #[tokio::test]
    async fn failed_settlement_closes_a_matched_tombstone_without_losing_confirmed_fills() {
        let tracked = tracked_orders();
        let mut matched_order = private_order("MATCHED");
        matched_order.timestamp = Some((now_micros() / 1_000_000) as i64);
        let private_events = private_order_events(matched_order, &tracked)
            .await
            .expect("MATCHED order update is tracked while settlement is pending");
        assert!(private_events.is_empty());
        let tracking = tracked.read().await.clone();
        assert!(tracking.active.is_empty());
        assert!(tracking.terminal_by_venue.contains_key("venue-1"));

        let mut confirmed_part = rest_trade("venue-1");
        confirmed_part.id = "trade-confirmed-part".to_string();
        confirmed_part.size = Decimal::new(25, 1);
        let mut failed_remainder = rest_trade("venue-1");
        failed_remainder.id = "trade-failed-remainder".to_string();
        failed_remainder.size = Decimal::new(25, 1);
        failed_remainder.status = TradeStatusType::Failed;

        let activity = account_activity_events(
            &[],
            // The REST API does not promise that final trade rows are ordered by accounting
            // semantics. Even when FAILED is observed first, confirmed fills must be staged before
            // the no-fill cancellation of the remainder.
            &[failed_remainder, confirmed_part],
            &tracking,
            api_key(),
            None,
        )
        .expect("a complete strict REST slice resolves FAILED as a no-fill terminal remainder");
        assert!(matches!(
            activity.events.as_slice(),
            [
                ExecutionEvent::Fill {
                    quantity: Quantity(quantity),
                    ..
                },
                ExecutionEvent::FeeCharged { .. },
                ExecutionEvent::OrderCanceled { order_id, .. }
            ] if *quantity == Decimal::new(25, 1) && order_id.0 == "logical-1"
        ));
        assert_eq!(activity.fill_updates.len(), 1);
        assert!(activity.failed_orders.contains_key("venue-1"));

        let mut replacement_tracking = tracking;
        replacement_tracking.activate(tracked_order("logical-1", "venue-2", "client-2"));
        let mut old_failed = rest_trade("venue-1");
        old_failed.status = TradeStatusType::Failed;
        let replacement_activity =
            account_activity_events(&[], &[old_failed], &replacement_tracking, api_key(), None)
                .expect("an old failed venue trade is final but must not cancel its replacement");
        assert!(replacement_activity.events.is_empty());
    }

    #[test]
    fn fill_deduper_evicts_oldest_id_at_capacity() {
        let mut seen = FillDeduper::with_capacity(2);
        assert!(seen.insert("fill-1".to_string()));
        assert!(seen.insert("fill-2".to_string()));
        assert!(!seen.insert("fill-1".to_string()));
        assert!(seen.insert("fill-3".to_string()));
        assert_eq!(seen.ids.len(), 2);
        assert!(seen.insert("fill-1".to_string()), "oldest ID was evicted");
    }

    #[tokio::test]
    async fn matched_order_state_terminalizes_active_and_closes_unfilled_fak_remainder() {
        let tracked = tracked_orders();
        let events = private_order_events(private_order("MATCHED"), &tracked)
            .await
            .unwrap();
        assert!(events.is_empty(), "the confirmed fill carries completion");
        assert!(tracked.read().await.active.is_empty());
        assert!(tracked
            .read()
            .await
            .terminal_by_venue
            .contains_key("venue-1"));

        let partial = tracked_orders();
        let events = private_order_events(private_order_with_progress("MATCHED", "2.5"), &partial)
            .await
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderCanceled { order_id, .. }] if order_id.0 == "logical-1"
        ));
        assert!(partial.read().await.active.is_empty());
    }

    #[tokio::test]
    async fn canceled_order_tombstone_maps_late_confirmed_fill() {
        let tracked = tracked_orders();
        tracked
            .write()
            .await
            .terminalize(&OrderId("logical-1".to_string()))
            .expect("active order becomes a terminal tombstone");

        let order_events = private_order_events(private_order("CANCELED"), &tracked)
            .await
            .expect("late cancellation is recognized");
        assert!(
            order_events.is_empty(),
            "terminal order updates are suppressed"
        );
        assert!(
            private_order_events(private_order_with_progress("LIVE", "0"), &tracked)
                .await
                .is_err(),
            "a terminal venue order returning live must require reconciliation"
        );

        let events = private_trade_events(
            private_trade("CONFIRMED", "123", "venue-1"),
            &tracked,
            &Arc::new(Mutex::new(FillDeduper::default())),
        )
        .await
        .expect("late confirmed trade resolves through the tombstone");
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::Fill { order_id, .. }, ExecutionEvent::FeeCharged { .. }]
                if order_id.0 == "logical-1"
        ));
    }

    #[tokio::test]
    async fn distinct_late_fills_cannot_exceed_canceled_order_remainder() {
        let tracked = tracked_orders();
        tracked
            .write()
            .await
            .terminalize(&OrderId("logical-1".to_string()))
            .expect("active order becomes a terminal tombstone");
        let seen = Arc::new(Mutex::new(FillDeduper::default()));

        let mut first = private_trade("CONFIRMED", "123", "venue-1");
        first.id = "late-fill-1".to_string();
        first.size = Decimal::from(3);
        private_trade_events(first, &tracked, &seen)
            .await
            .expect("first late fill fits within the canceled remainder");
        assert_eq!(
            tracked
                .read()
                .await
                .terminal_by_venue
                .get("venue-1")
                .expect("terminal alias remains available")
                .order
                .remaining_quantity,
            Decimal::from(2)
        );

        let mut overfill = private_trade("CONFIRMED", "123", "venue-1");
        overfill.id = "late-fill-2".to_string();
        overfill.size = Decimal::from(3);
        assert!(private_trade_events(overfill, &tracked, &seen)
            .await
            .is_err());
        assert!(
            !seen.lock().await.contains("late-fill-2:venue-1"),
            "failed overfill must not commit its fill ID to dedupe state"
        );
        assert_eq!(
            tracked
                .read()
                .await
                .terminal_by_venue
                .get("venue-1")
                .expect("failed overfill does not remove the terminal alias")
                .order
                .remaining_quantity,
            Decimal::from(2),
            "failed overfill must not partially mutate tracking state"
        );
    }

    #[test]
    fn terminal_tombstones_are_capacity_and_time_bounded() {
        let mut tracking = TrackingBook::default();
        for index in 0..=MAX_TERMINAL_TOMBSTONES {
            tracking.insert_terminal(
                tracked_order(
                    &format!("logical-{index}"),
                    &format!("venue-{index}"),
                    &format!("client-{index}"),
                ),
                index as u64 + 1,
            );
        }

        assert_eq!(tracking.terminal_by_venue.len(), MAX_TERMINAL_TOMBSTONES);
        assert!(!tracking.terminal_by_venue.contains_key("venue-0"));

        tracking.prune_terminal(TERMINAL_TOMBSTONE_TTL_US + MAX_TERMINAL_TOMBSTONES as u64 + 2);
        assert!(tracking.terminal_by_venue.is_empty());
    }

    #[test]
    fn stale_reconciliation_cannot_terminalize_or_update_a_replacement() {
        let mut tracking = tracked_orders().blocking_read().clone();
        tracking.activate(tracked_order("logical-1", "venue-2", "client-2"));
        let logical_id = OrderId("logical-1".to_string());

        assert!(tracking
            .terminalize_reconciled(&logical_id, "venue-1")
            .is_err());
        assert!(tracking
            .update_reconciled_remaining(&logical_id, "venue-1", Decimal::ONE)
            .is_err());
        assert_eq!(
            tracking
                .active_order(&logical_id)
                .expect("replacement remains active")
                .venue_id,
            "venue-2"
        );
    }

    #[tokio::test]
    async fn submission_guard_installs_replacement_alias_before_private_reader_resumes() {
        let tracked = Arc::new(RwLock::new(TrackingBook::default()));
        let mut guard = tracked.write().await;
        let private_reader = tokio::spawn({
            let tracked = Arc::clone(&tracked);
            async move { private_order_events(private_order_with_progress("LIVE", "0"), &tracked).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !private_reader.is_finished(),
            "private reader must wait for the submission alias"
        );

        let envelope = OrderIntentEnvelope::new(
            intent("123"),
            tracked_order("logical-1", "venue-1", "unused").lifecycle,
        )
        .with_client_order_id("replacement-client-2");
        activate_submission(
            &mut guard,
            &envelope,
            OrderId("logical-1".to_string()),
            "venue-1".to_string(),
            1,
        );
        drop(guard);

        let events = private_reader
            .await
            .expect("private reader task")
            .expect("tracked private order");
        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::OrderAck { order_id, .. }] if order_id.0 == "logical-1"
        ));
        assert_eq!(
            tracked
                .read()
                .await
                .active_order(&OrderId("logical-1".to_string()))
                .and_then(|order| order.client_order_id.as_deref()),
            Some("replacement-client-2")
        );
    }

    #[tokio::test]
    async fn pre_subscription_private_event_is_buffered_until_receiver_attaches() {
        let (event_tx, _) = broadcast::channel(4);
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        let healthy = Arc::new(AtomicBool::new(true));
        emit_event(
            &event_tx,
            &pending,
            &healthy,
            ExecutionEvent::ConnectionStatus {
                connected: true,
                timestamp: 1,
            },
        )
        .await;
        assert_eq!(pending.lock().await.len(), 1);

        let mut receiver = event_tx.subscribe();
        let buffered = pending.lock().await.pop_front().expect("buffered event");
        assert!(matches!(
            buffered,
            ExecutionEvent::ConnectionStatus {
                connected: true,
                ..
            }
        ));
        emit_event(
            &event_tx,
            &pending,
            &healthy,
            ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: 2,
            },
        )
        .await;
        assert!(matches!(
            receiver.recv().await.expect("live event"),
            ExecutionEvent::ConnectionStatus {
                connected: false,
                ..
            }
        ));
    }

    #[test]
    fn account_catch_up_rejects_unmapped_activity_with_empty_tracking() {
        let tracking = TrackingBook::default();
        let recovery_unaccounted_fill = AtomicBool::new(false);

        assert!(account_activity_events(
            &[rest_open_order("external-order")],
            &[],
            &tracking,
            api_key(),
            None,
        )
        .is_err());
        assert!(account_activity_events(
            &[],
            &[rest_trade("external-order")],
            &tracking,
            api_key(),
            Some(&recovery_unaccounted_fill),
        )
        .is_err());
        assert!(
            recovery_unaccounted_fill.load(Ordering::Acquire),
            "a REST-only confirmed fill must permanently block automatic recovery"
        );
    }

    #[test]
    fn account_catch_up_recovers_fast_confirmed_fill_after_tracking_is_installed() {
        let tracking = tracked_orders().blocking_read().clone();
        let events =
            account_activity_events(&[], &[rest_trade("venue-1")], &tracking, api_key(), None)
                .expect("tracked submit response makes the missed private fill recoverable")
                .events;

        assert!(matches!(
            events.as_slice(),
            [ExecutionEvent::Fill { order_id, .. }, ExecutionEvent::FeeCharged { .. }]
                if order_id.0 == "logical-1"
        ));
    }
}
