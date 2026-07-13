use alloy::signers::local::PrivateKeySigner;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use ploy_market_contracts::{FeeSchedule, LiquidityRole};
use ploy_trading::{FillRecord, TradeSide};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{Normal, Signer};
use polymarket_client_sdk::clob::types::request::{BalanceAllowanceRequest, TradesRequest};
use polymarket_client_sdk::clob::types::response::{MakerOrder, OpenOrderResponse, TradeResponse};
use polymarket_client_sdk::clob::types::{
    Amount, AssetType, OrderStatusType, OrderType, Side, SignatureType, TradeStatusType,
};
use polymarket_client_sdk::clob::ws::types::response::{OrderMessageType, TradeMessageStatus};
use polymarket_client_sdk::clob::ws::{
    ChannelType, Client as WsClient, OrderMessage, TradeMessage, WsMessage,
};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{Address, B256, U256};
use polymarket_client_sdk::{
    contract_config, derive_proxy_wallet, derive_safe_wallet, POLYGON, PRIVATE_KEY_VAR,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use secrecy::{ExposeSecret, SecretString};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc;

pub const CRATE_MARKER: &str = "ploy-connectivity";
const DEFAULT_POLY_CLOB_HOST: &str = "https://clob.polymarket.com";
const TERMINAL_CURSOR: &str = "LTE=";
const MAX_CONCURRENT_TRADE_RECONCILE_REQUESTS: usize = 10;
const TRADE_RECONCILE_LOOKBACK_SECS: u64 = 24 * 60 * 60;
const TRADE_RECONCILE_TIMEOUT: Duration = Duration::from_secs(2);
const FEE_SCHEDULE_TTL: Duration = Duration::from_secs(5 * 60);
const USER_EVENT_QUEUE_CAPACITY: usize = 4_096;
const USER_EVENT_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const USER_EVENT_HEALTH_INTERVAL: Duration = Duration::from_millis(100);
const USER_EVENT_PERIODIC_CATCH_UP: Duration = Duration::from_secs(5);
const CONDITIONAL_TOKEN_DECIMALS: u32 = 6;
const ACCOUNT_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

/// Order execution type for live trading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderExecutionType {
    /// Good-Til-Cancelled (rests on book).
    GTC,
    /// Fill-and-Kill (fill what you can, cancel rest). Default for 5-min markets.
    #[default]
    FAK,
    /// Fill-or-Kill (fill entirely or cancel entirely).
    FOK,
}

impl OrderExecutionType {
    pub fn into_sdk(self) -> OrderType {
        match self {
            Self::GTC => OrderType::GTC,
            Self::FAK => OrderType::FAK,
            Self::FOK => OrderType::FOK,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRequest {
    pub order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    /// Order execution type (default: FAK for immediate execution).
    pub order_type: OrderExecutionType,
    /// Extra ticks above best ask for aggressive pricing (0 = use exact limit_price).
    pub aggressive_ticks: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedOrder {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
    pub side: TradeSide,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OrderObservation {
    Acknowledged {
        order_id: String,
        venue_order_id: String,
    },
    Canceled {
        order_id: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileBatch {
    pub fills: Vec<FillRecord>,
    pub order_observations: Vec<OrderObservation>,
}

impl ReconcileBatch {
    #[must_use]
    pub fn fills_only(fills: Vec<FillRecord>) -> Self {
        Self {
            fills,
            order_observations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationRequest {
    pub order_id: String,
    pub venue_order_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceRequest {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    Acknowledged { venue_order_id: String },
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CancellationOutcome {
    Canceled,
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplaceOutcome {
    Replaced { venue_order_id: String },
    Rejected { reason: String },
    PartialFailure { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutionError {
    #[error("live execution misconfigured: {0}")]
    Configuration(String),
    #[error("live execution validation failed: {0}")]
    Validation(String),
    #[error("live execution transport failed: {0}")]
    Transport(String),
}

pub trait LiveExecutionGateway: Send + Sync + std::fmt::Debug {
    fn probe(&self) -> Result<(), ExecutionError>;

    fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError>;

    fn cancel(&self, request: &CancellationRequest) -> Result<CancellationOutcome, ExecutionError>;

    fn replace(&self, request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError>;

    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError>;

    fn reconcile_updates(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<ReconcileBatch, ExecutionError> {
        self.reconcile_fills(tracked_orders)
            .map(ReconcileBatch::fills_only)
    }
}

#[derive(Debug, Clone)]
pub struct StaticExecutionGateway {
    probe_result: Result<(), ExecutionError>,
    result: Result<ExecutionOutcome, ExecutionError>,
    cancel_result: Result<CancellationOutcome, ExecutionError>,
    replace_result: Result<ReplaceOutcome, ExecutionError>,
    reconcile_result: Result<ReconcileBatch, ExecutionError>,
}

impl StaticExecutionGateway {
    pub fn acknowledged(venue_order_id: impl Into<String>) -> Self {
        let venue_order_id = venue_order_id.into();
        Self {
            probe_result: Ok(()),
            result: Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: venue_order_id.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Replaced {
                venue_order_id: format!("{venue_order_id}-replaced"),
            }),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    pub fn rejected(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            probe_result: Ok(()),
            result: Ok(ExecutionOutcome::Rejected {
                reason: reason.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Rejected { reason }),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    pub fn failed(error: ExecutionError) -> Self {
        Self {
            probe_result: Ok(()),
            result: Err(error.clone()),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Err(error),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    pub fn with_cancel_result(
        mut self,
        result: Result<CancellationOutcome, ExecutionError>,
    ) -> Self {
        self.cancel_result = result;
        self
    }

    pub fn with_probe_result(mut self, result: Result<(), ExecutionError>) -> Self {
        self.probe_result = result;
        self
    }

    pub fn with_replace_result(mut self, result: Result<ReplaceOutcome, ExecutionError>) -> Self {
        self.replace_result = result;
        self
    }

    pub fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
        self.reconcile_result = Ok(ReconcileBatch::fills_only(fills));
        self
    }

    pub fn with_reconciled_updates(mut self, updates: ReconcileBatch) -> Self {
        self.reconcile_result = Ok(updates);
        self
    }

    pub fn with_reconcile_result(
        mut self,
        result: Result<Vec<FillRecord>, ExecutionError>,
    ) -> Self {
        self.reconcile_result = result.map(ReconcileBatch::fills_only);
        self
    }
}

impl LiveExecutionGateway for StaticExecutionGateway {
    fn probe(&self) -> Result<(), ExecutionError> {
        self.probe_result.clone()
    }

    fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        self.result.clone()
    }

    fn cancel(
        &self,
        _request: &CancellationRequest,
    ) -> Result<CancellationOutcome, ExecutionError> {
        self.cancel_result.clone()
    }

    fn reconcile_fills(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        self.reconcile_result.clone().map(|batch| batch.fills)
    }

    fn reconcile_updates(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<ReconcileBatch, ExecutionError> {
        self.reconcile_result.clone()
    }

    fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        self.replace_result.clone()
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
    fn into_sdk(self) -> SignatureType {
        match self {
            Self::Eoa => SignatureType::Eoa,
            Self::Proxy => SignatureType::Proxy,
            Self::GnosisSafe => SignatureType::GnosisSafe,
            Self::Poly1271 => SignatureType::Poly1271,
        }
    }
}

impl Default for WalletSignatureType {
    fn default() -> Self {
        Self::Eoa
    }
}

impl FromStr for WalletSignatureType {
    type Err = ExecutionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "eoa" => Ok(Self::Eoa),
            "proxy" => Ok(Self::Proxy),
            "gnosis_safe" => Ok(Self::GnosisSafe),
            "poly1271" => Ok(Self::Poly1271),
            other => Err(ExecutionError::Configuration(format!(
                "unsupported POLY_SIGNATURE_TYPE `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PolymarketExecutionConfig {
    pub host: String,
    pub private_key: Option<SecretString>,
    pub use_server_time: bool,
    pub funder: Option<String>,
    pub signature_type: WalletSignatureType,
}

impl Default for PolymarketExecutionConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_POLY_CLOB_HOST.to_string(),
            private_key: None,
            use_server_time: true,
            signature_type: WalletSignatureType::Eoa,
            funder: None,
        }
    }
}

impl PolymarketExecutionConfig {
    pub fn from_env() -> Result<Self, ExecutionError> {
        let mut config = Self::default();
        config.private_key = polymarket_private_key_from_env().map(SecretString::from);
        config.funder = polymarket_funder_from_env();
        config.signature_type = polymarket_signature_type_from_env(config.funder.is_some())?;

        if let Ok(value) = std::env::var("POLY_CLOB_HOST") {
            config.host = value;
        }
        if let Ok(value) = std::env::var("POLY_USE_SERVER_TIME") {
            config.use_server_time = matches!(value.as_str(), "1" | "true" | "TRUE" | "yes");
        }

        Ok(config)
    }
}

pub fn polymarket_execution_principal_from_env() -> Result<String, ExecutionError> {
    polymarket_execution_principal(&PolymarketExecutionConfig::from_env()?)
}

pub fn polymarket_execution_principal(
    config: &PolymarketExecutionConfig,
) -> Result<String, ExecutionError> {
    let signer = signer_from_config(config)?;
    match config.signature_type {
        WalletSignatureType::Eoa => Ok(format!("{:#x}", signer.address())),
        WalletSignatureType::Proxy
        | WalletSignatureType::GnosisSafe
        | WalletSignatureType::Poly1271 => {
            let address = validated_funder(config, signer.address())?.expect("non-EOA funder");
            Ok(format!("{address:#x}"))
        }
    }
}

fn signer_from_config(
    config: &PolymarketExecutionConfig,
) -> Result<PrivateKeySigner, ExecutionError> {
    let private_key = config
        .private_key
        .as_ref()
        .map(ExposeSecret::expose_secret)
        .ok_or_else(|| {
            ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
        })?;
    PrivateKeySigner::from_str(private_key)
        .map_err(|error| {
            ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {error}"))
        })
        .map(|signer| signer.with_chain_id(Some(POLYGON)))
}

fn validated_funder(
    config: &PolymarketExecutionConfig,
    signer: Address,
) -> Result<Option<Address>, ExecutionError> {
    let configured = config
        .funder
        .as_deref()
        .map(Address::from_str)
        .transpose()
        .map_err(|error| ExecutionError::Configuration(format!("invalid POLY_FUNDER: {error}")))?;
    let derived = match config.signature_type {
        WalletSignatureType::Eoa => {
            if configured.is_some() {
                return Err(ExecutionError::Configuration(
                    "POLY_FUNDER must not be set for eoa signatures".to_string(),
                ));
            }
            return Ok(None);
        }
        WalletSignatureType::Proxy => derive_proxy_wallet(signer, POLYGON),
        WalletSignatureType::GnosisSafe => derive_safe_wallet(signer, POLYGON),
        WalletSignatureType::Poly1271 => {
            let funder = configured
                .filter(|address| *address != Address::ZERO)
                .ok_or_else(|| {
                    ExecutionError::Configuration(
                        "POLY_FUNDER is required for poly1271 signatures".to_string(),
                    )
                })?;
            return Ok(Some(funder));
        }
    }
    .ok_or_else(|| {
        ExecutionError::Configuration("wallet derivation is unavailable on Polygon".to_string())
    })?;

    if configured.is_some_and(|configured| configured != derived) {
        return Err(ExecutionError::Configuration(format!(
            "POLY_FUNDER does not match the signer-derived {:?} wallet {derived:#x}",
            config.signature_type
        )));
    }
    Ok(Some(derived))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolymarketAccountReadiness {
    pub principal: String,
    pub required_pusd: Decimal,
    pub balance_pusd: Decimal,
    pub country: String,
    pub region: String,
}

pub fn polymarket_account_readiness_from_env(
    required_pusd: Decimal,
) -> Result<PolymarketAccountReadiness, ExecutionError> {
    PolymarketExecutionGateway::from_env()?.account_readiness(required_pusd)
}

#[derive(Debug)]
enum BufferedUserEvent {
    Order(OrderMessage),
    Trade(TradeMessage),
}

#[derive(Debug)]
struct UserEventStream {
    sender: mpsc::Sender<BufferedUserEvent>,
    receiver: Mutex<mpsc::Receiver<BufferedUserEvent>>,
    started: AtomicBool,
    connected: AtomicBool,
    gap_detected: AtomicBool,
    last_catch_up: Mutex<Instant>,
}

impl UserEventStream {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel(USER_EVENT_QUEUE_CAPACITY);
        Self {
            sender,
            receiver: Mutex::new(receiver),
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            gap_detected: AtomicBool::new(true),
            last_catch_up: Mutex::new(Instant::now()),
        }
    }

    fn enqueue(&self, event: BufferedUserEvent) {
        if self.sender.try_send(event).is_err() {
            self.gap_detected.store(true, Ordering::Release);
        }
    }

    fn drain(&self) -> Result<Vec<BufferedUserEvent>, ExecutionError> {
        let mut receiver = self
            .receiver
            .lock()
            .map_err(|_| ExecutionError::Transport("user event queue poisoned".to_string()))?;
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        Ok(events)
    }

    fn begin_catch_up(&self) -> Result<bool, ExecutionError> {
        let periodic_catch_up_due = self
            .last_catch_up
            .lock()
            .map_err(|_| ExecutionError::Transport("user event catch-up clock poisoned".into()))?
            .elapsed()
            >= USER_EVENT_PERIODIC_CATCH_UP;
        let gap_detected = self.gap_detected.swap(false, Ordering::AcqRel);
        Ok(gap_detected || !self.connected.load(Ordering::Acquire) || periodic_catch_up_due)
    }

    fn finish_catch_up(&self, succeeded: bool) {
        if succeeded {
            if let Ok(mut last_catch_up) = self.last_catch_up.lock() {
                *last_catch_up = Instant::now();
            }
        }
        if !succeeded || !self.connected.load(Ordering::Acquire) {
            self.gap_detected.store(true, Ordering::Release);
        }
    }
}

#[derive(Debug, Clone)]
pub struct PolymarketExecutionGateway {
    config: PolymarketExecutionConfig,
    runtime: Arc<tokio::runtime::Runtime>,
    signer: Arc<OnceLock<PrivateKeySigner>>,
    client: Arc<RwLock<Option<Client<Authenticated<Normal>>>>>,
    fee_schedules: Arc<RwLock<HashMap<U256, (FeeSchedule, Instant)>>>,
    user_events: Arc<UserEventStream>,
}

impl PolymarketExecutionGateway {
    pub fn from_env() -> Result<Self, ExecutionError> {
        Ok(Self::new(PolymarketExecutionConfig::from_env()?))
    }

    pub fn new(config: PolymarketExecutionConfig) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("ploy-polymarket-gateway")
            .build()
            .expect("create Polymarket execution runtime");

        Self {
            config,
            runtime: Arc::new(runtime),
            signer: Arc::new(OnceLock::new()),
            client: Arc::new(RwLock::new(None)),
            fee_schedules: Arc::new(RwLock::new(HashMap::new())),
            user_events: Arc::new(UserEventStream::new()),
        }
    }

    fn get_or_init_signer(&self) -> Result<PrivateKeySigner, ExecutionError> {
        if let Some(signer) = self.signer.get() {
            return Ok(signer.clone());
        }

        let signer = signer_from_config(&self.config)?;

        let _ = self.signer.set(signer);
        Ok(self.signer.get().expect("signer set above").clone())
    }

    fn get_or_init_client(&self) -> Result<Client<Authenticated<Normal>>, ExecutionError> {
        if let Some(client) = self
            .client
            .read()
            .map_err(|_| ExecutionError::Transport("client cache poisoned".to_string()))?
            .clone()
        {
            return Ok(client.clone());
        }

        let client = self.authenticate_client()?;

        let mut guard = self
            .client
            .write()
            .map_err(|_| ExecutionError::Transport("client cache poisoned".to_string()))?;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        *guard = Some(client.clone());
        Ok(client)
    }

    fn authenticate_client(&self) -> Result<Client<Authenticated<Normal>>, ExecutionError> {
        let signer = self.get_or_init_signer()?;
        let funder = validated_funder(&self.config, signer.address())?;
        self.runtime.block_on(async {
            let client = Client::new(
                &self.config.host,
                Config::builder()
                    .use_server_time(self.config.use_server_time)
                    .build(),
            )
            .map_err(|err| ExecutionError::Transport(format!("build client: {err}")))?;

            let mut auth = client.authentication_builder(&signer);
            auth = auth.signature_type(self.config.signature_type.into_sdk());
            if let Some(funder) = funder {
                auth = auth.funder(funder);
            }

            auth.authenticate()
                .await
                .map_err(|err| ExecutionError::Transport(format!("authenticate client: {err}")))
        })
    }

    fn ensure_user_event_stream(&self, client: &Client<Authenticated<Normal>>) {
        if self
            .user_events
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let credentials = client.credentials().clone();
        let address = client.address();
        let state = Arc::clone(&self.user_events);
        self.runtime.spawn(async move {
            run_user_event_stream(credentials, address, state).await;
        });
    }

    fn clear_client_cache(&self) {
        if let Ok(mut guard) = self.client.write() {
            *guard = None;
        }
    }

    fn maybe_clear_client_cache(&self, error: &ExecutionError) {
        if is_auth_recovery_error(error) {
            self.clear_client_cache();
        }
    }

    pub fn account_readiness(
        &self,
        required_pusd: Decimal,
    ) -> Result<PolymarketAccountReadiness, ExecutionError> {
        let required_raw = pusd_to_raw(required_pusd)?;
        let principal = polymarket_execution_principal(&self.config)?;
        let signer = self.get_or_init_signer()?;
        let funder = validated_funder(&self.config, signer.address())?;
        let (geoblock, closed_only, balance_allowance) = self.runtime.block_on(async {
            tokio::time::timeout(ACCOUNT_READINESS_TIMEOUT, async {
                let client = Client::new(
                    &self.config.host,
                    Config::builder()
                        .use_server_time(self.config.use_server_time)
                        .heartbeat_interval(Duration::from_secs(60))
                        .build(),
                )
                .map_err(|error| {
                    ExecutionError::Transport(format!("build readiness client: {error}"))
                })?;
                let credentials = client
                    .derive_api_key(&signer, None)
                    .await
                    .map_err(|error| {
                        ExecutionError::Transport(format!(
                            "derive existing Polymarket API credentials: {error}"
                        ))
                    })?;
                let mut auth = client
                    .authentication_builder(&signer)
                    .credentials(credentials)
                    .signature_type(self.config.signature_type.into_sdk());
                if let Some(funder) = funder {
                    auth = auth.funder(funder);
                }
                let mut client = auth.authenticate().await.map_err(|error| {
                    ExecutionError::Transport(format!("authenticate readiness client: {error}"))
                })?;
                client.stop_heartbeats().await.map_err(|error| {
                    ExecutionError::Transport(format!("stop readiness-client heartbeats: {error}"))
                })?;
                if client.heartbeats_active() {
                    return Err(ExecutionError::Transport(
                        "readiness client heartbeat task is still active".to_string(),
                    ));
                }
                let version = client.version().await.map_err(|error| {
                    ExecutionError::Transport(format!("CLOB version probe: {error}"))
                })?;
                if version != 2 {
                    return Err(ExecutionError::Validation(format!(
                        "Polymarket CLOB API v2 is required; host reported v{version}"
                    )));
                }
                let geoblock = client.check_geoblock().await.map_err(|error| {
                    ExecutionError::Transport(format!("geoblock probe: {error}"))
                })?;
                let closed_only = client.closed_only_mode().await.map_err(|error| {
                    ExecutionError::Transport(format!("closed-only probe: {error}"))
                })?;
                let balance_allowance = client
                    .balance_allowance(
                        BalanceAllowanceRequest::builder()
                            .asset_type(AssetType::Collateral)
                            .build(),
                    )
                    .await
                    .map_err(|error| {
                        ExecutionError::Transport(format!(
                            "collateral balance/allowance probe: {error}"
                        ))
                    })?;
                Ok::<_, ExecutionError>((geoblock, closed_only, balance_allowance))
            })
            .await
            .map_err(|_| {
                ExecutionError::Transport(format!(
                    "Polymarket account readiness timed out after {} seconds",
                    ACCOUNT_READINESS_TIMEOUT.as_secs()
                ))
            })?
        })?;

        let balance_raw = collateral_balance_to_raw(balance_allowance.balance)?;
        let standard_exchange = contract_config(POLYGON, false)
            .and_then(|config| config.exchange_v2)
            .ok_or_else(|| {
                ExecutionError::Configuration(
                    "missing Polygon standard V2 exchange contract".to_string(),
                )
            })?;
        let neg_risk_exchange = contract_config(POLYGON, true)
            .and_then(|config| config.exchange_v2)
            .ok_or_else(|| {
                ExecutionError::Configuration(
                    "missing Polygon neg-risk V2 exchange contract".to_string(),
                )
            })?;
        let standard_allowance = balance_allowance
            .allowances
            .get(&standard_exchange)
            .map(String::as_str)
            .unwrap_or("0");
        let neg_risk_allowance = balance_allowance
            .allowances
            .get(&neg_risk_exchange)
            .map(String::as_str)
            .unwrap_or("0");
        ensure_account_readiness(
            geoblock.blocked,
            closed_only.closed_only,
            balance_raw,
            standard_allowance,
            neg_risk_allowance,
            required_raw,
        )?;

        Ok(PolymarketAccountReadiness {
            principal,
            required_pusd,
            balance_pusd: raw_balance_to_pusd(balance_raw)?,
            country: geoblock.country,
            region: geoblock.region,
        })
    }
}

async fn run_user_event_stream(
    credentials: polymarket_client_sdk::auth::Credentials,
    address: Address,
    state: Arc<UserEventStream>,
) {
    loop {
        let ws = match WsClient::default().authenticate(credentials.clone(), address) {
            Ok(client) => client,
            Err(error) => {
                state.connected.store(false, Ordering::Release);
                state.gap_detected.store(true, Ordering::Release);
                tracing::warn!(%error, "authenticate Polymarket user WebSocket failed");
                tokio::time::sleep(USER_EVENT_RECONNECT_DELAY).await;
                continue;
            }
        };
        let stream = match ws.subscribe_user_events(Vec::new()) {
            Ok(stream) => stream,
            Err(error) => {
                state.connected.store(false, Ordering::Release);
                state.gap_detected.store(true, Ordering::Release);
                tracing::warn!(%error, "subscribe Polymarket user WebSocket failed");
                tokio::time::sleep(USER_EVENT_RECONNECT_DELAY).await;
                continue;
            }
        };
        let mut stream = std::pin::pin!(stream);
        let mut health = tokio::time::interval(USER_EVENT_HEALTH_INTERVAL);
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                message = stream.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        Ok(WsMessage::Trade(trade)) => {
                            if matches!(
                                trade.status,
                                TradeMessageStatus::Failed | TradeMessageStatus::Unknown(_)
                            ) {
                                state.gap_detected.store(true, Ordering::Release);
                            }
                            state.enqueue(BufferedUserEvent::Trade(trade));
                        }
                        Ok(WsMessage::Order(order)) => {
                            if matches!(order.msg_type, Some(OrderMessageType::Unknown(_)))
                                || matches!(
                                    order.status,
                                    Some(OrderStatusType::Delayed | OrderStatusType::Unknown(_))
                                )
                            {
                                state.gap_detected.store(true, Ordering::Release);
                            }
                            state.enqueue(BufferedUserEvent::Order(order));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            state.connected.store(false, Ordering::Release);
                            state.gap_detected.store(true, Ordering::Release);
                            tracing::warn!(%error, "Polymarket user WebSocket stream error");
                            break;
                        }
                    }
                }
                _ = health.tick() => {
                    let connected = ws.connection_state(ChannelType::User).is_connected();
                    let was_connected = state.connected.swap(connected, Ordering::AcqRel);
                    if was_connected && !connected {
                        state.gap_detected.store(true, Ordering::Release);
                    }
                }
            }
        }

        state.connected.store(false, Ordering::Release);
        state.gap_detected.store(true, Ordering::Release);
        tokio::time::sleep(USER_EVENT_RECONNECT_DELAY).await;
    }
}

impl LiveExecutionGateway for PolymarketExecutionGateway {
    fn probe(&self) -> Result<(), ExecutionError> {
        let client = self.get_or_init_client()?;
        let result = self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), client.server_time())
                .await
                .map_err(|_| ExecutionError::Transport("venue health probe timed out".to_string()))?
                .map(|_| ())
                .map_err(|err| ExecutionError::Transport(format!("venue health probe: {err}")))
        });
        if let Err(error) = &result {
            self.maybe_clear_client_cache(error);
        }
        result
    }

    fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        let limit_price = request.limit_price.ok_or_else(|| {
            ExecutionError::Validation(
                "live Polymarket execution currently requires a limit price".to_string(),
            )
        })?;
        let token_id = U256::from_str(&request.token_id).map_err(|err| {
            ExecutionError::Validation(format!("invalid token_id `{}`: {err}", request.token_id))
        })?;

        let client = self.get_or_init_client()?;
        let signer = self.get_or_init_signer()?;

        let result = self.runtime.block_on(async {
            let side = polymarket_side(request.side);
            let quantity = if request.side == TradeSide::Sell {
                sell_quantity_capped_to_balance(&client, token_id, request.quantity).await?
            } else {
                request.quantity
            };
            let order = match request.order_type {
                OrderExecutionType::GTC => {
                    let price = execution_price_override(
                        request.order_type,
                        request.side,
                        limit_price,
                        request.aggressive_ticks,
                    )
                    .expect("GTC orders must keep an explicit price");
                    let normalized_quantity = normalize_order_quantity(quantity);
                    client
                        .limit_order()
                        .token_id(token_id)
                        .order_type(request.order_type.into_sdk())
                        .price(price)
                        .size(normalized_quantity)
                        .side(side)
                        .build()
                        .await
                }
                OrderExecutionType::FAK | OrderExecutionType::FOK => {
                    let price = execution_price_override(
                        request.order_type,
                        request.side,
                        limit_price,
                        request.aggressive_ticks,
                    )
                    .unwrap_or(limit_price);
                    let amount =
                        normalize_execution_amount(quantity, limit_price, side).map_err(|err| {
                            ExecutionError::Validation(format!("build amount: {err}"))
                        })?;
                    client
                        .market_order()
                        .token_id(token_id)
                        .order_type(request.order_type.into_sdk())
                        .price(price)
                        .amount(amount)
                        .side(side)
                        .build()
                        .await
                }
            }
            .map_err(|err| ExecutionError::Validation(format!("build limit order: {err}")))?;

            let signed_order = client
                .sign(&signer, order)
                .await
                .map_err(|err| ExecutionError::Transport(format!("sign order: {err}")))?;

            let response = client
                .post_order(signed_order)
                .await
                .map_err(|err| ExecutionError::Transport(format!("submit order: {err}")))?;

            if response.success {
                Ok(ExecutionOutcome::Acknowledged {
                    venue_order_id: response.order_id,
                })
            } else {
                Ok(ExecutionOutcome::Rejected {
                    reason: response.error_msg.unwrap_or_else(|| {
                        format!("venue rejected order with status {}", response.status)
                    }),
                })
            }
        });

        if let Err(error) = &result {
            self.maybe_clear_client_cache(error);
        }
        result
    }

    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        if tracked_orders.is_empty() {
            return Ok(Vec::new());
        }

        let client = self.get_or_init_client()?;

        let result = self.runtime.block_on(async {
            tokio::time::timeout(
                TRADE_RECONCILE_TIMEOUT,
                reconcile_rest_fills(&client, &self.fee_schedules, tracked_orders),
            )
            .await
            .map_err(|_| {
                ExecutionError::Transport(format!(
                    "trade reconciliation timed out after {}ms",
                    TRADE_RECONCILE_TIMEOUT.as_millis()
                ))
            })?
        });

        if let Err(error) = &result {
            self.maybe_clear_client_cache(error);
        }
        result
    }

    fn reconcile_updates(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<ReconcileBatch, ExecutionError> {
        if tracked_orders.is_empty() {
            return Ok(ReconcileBatch::default());
        }

        let client = self.get_or_init_client()?;
        self.ensure_user_event_stream(&client);

        let events = self.user_events.drain()?;
        let catch_up_required = self.user_events.begin_catch_up()?;
        let result = self.runtime.block_on(async {
            let mut batch =
                reconcile_user_events(&client, &self.fee_schedules, tracked_orders, events).await?;

            if catch_up_required {
                let rest_batch = tokio::time::timeout(
                    TRADE_RECONCILE_TIMEOUT,
                    reconcile_rest_updates(&client, &self.fee_schedules, tracked_orders),
                )
                .await
                .map_err(|_| {
                    ExecutionError::Transport(format!(
                        "trade reconciliation timed out after {}ms",
                        TRADE_RECONCILE_TIMEOUT.as_millis()
                    ))
                })??;
                batch.fills.extend(rest_batch.fills);
                batch
                    .order_observations
                    .extend(rest_batch.order_observations);
            }

            deduplicate_fills(&mut batch.fills);
            deduplicate_order_observations(&mut batch.order_observations);
            Ok(batch)
        });

        if catch_up_required {
            self.user_events.finish_catch_up(result.is_ok());
        } else if result.is_err() {
            self.user_events.gap_detected.store(true, Ordering::Release);
        }
        if let Err(error) = &result {
            self.maybe_clear_client_cache(error);
        }
        result
    }

    fn cancel(&self, request: &CancellationRequest) -> Result<CancellationOutcome, ExecutionError> {
        let client = self.get_or_init_client()?;

        let result = self.runtime.block_on(async {
            let response = client
                .cancel_order(&request.venue_order_id)
                .await
                .map_err(|err| ExecutionError::Transport(format!("cancel order: {err}")))?;

            if response
                .canceled
                .iter()
                .any(|order_id| order_id == &request.venue_order_id)
            {
                Ok(CancellationOutcome::Canceled)
            } else if let Some(reason) = response.not_canceled.get(&request.venue_order_id) {
                Ok(CancellationOutcome::Rejected {
                    reason: reason.clone(),
                })
            } else {
                Ok(CancellationOutcome::Rejected {
                    reason: format!(
                        "venue did not confirm cancellation for order `{}`",
                        request.venue_order_id
                    ),
                })
            }
        });

        if let Err(error) = &result {
            self.maybe_clear_client_cache(error);
        }
        result
    }

    fn replace(&self, request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        match self.cancel(&CancellationRequest {
            order_id: request.order_id.clone(),
            venue_order_id: request.venue_order_id.clone(),
        })? {
            CancellationOutcome::Canceled => {}
            CancellationOutcome::Rejected { reason } => {
                return Ok(ReplaceOutcome::Rejected { reason });
            }
        }

        match self.submit(&ExecutionRequest {
            order_id: request.order_id.clone(),
            token_id: request.token_id.clone(),
            side: request.side,
            quantity: request.quantity,
            limit_price: request.limit_price,
            order_type: OrderExecutionType::GTC,
            aggressive_ticks: 0,
        })? {
            ExecutionOutcome::Acknowledged { venue_order_id } => {
                Ok(ReplaceOutcome::Replaced { venue_order_id })
            }
            ExecutionOutcome::Rejected { reason } => Ok(ReplaceOutcome::PartialFailure { reason }),
        }
    }
}

fn polymarket_side(side: TradeSide) -> Side {
    match side {
        TradeSide::Buy => Side::Buy,
        TradeSide::Sell => Side::Sell,
    }
}

fn execution_price_override(
    order_type: OrderExecutionType,
    side: TradeSide,
    limit_price: Decimal,
    aggressive_ticks: u8,
) -> Option<Decimal> {
    match order_type {
        OrderExecutionType::GTC => {
            let tick_size = dec!(0.01);
            Some(normalize_aggressive_price(
                limit_price,
                side,
                aggressive_ticks,
                tick_size,
            ))
        }
        OrderExecutionType::FAK | OrderExecutionType::FOK => Some(limit_price),
    }
}

fn first_env_value(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| std::env::var(key).ok())
}

fn polymarket_private_key_from_env() -> Option<String> {
    first_env_value(&[PRIVATE_KEY_VAR, "PRIVATE_KEY"])
}

fn polymarket_funder_from_env() -> Option<String> {
    first_env_value(&[
        "POLY_FUNDER",
        "POLYMARKET_FUNDER",
        "POLYMARKET_FUNDER_ADDRESS",
    ])
}

fn wallet_signature_type(
    value: Option<&str>,
    has_funder: bool,
) -> Result<WalletSignatureType, ExecutionError> {
    value
        .map(WalletSignatureType::from_str)
        .transpose()
        .map(|value| {
            value.unwrap_or(if has_funder {
                WalletSignatureType::Proxy
            } else {
                WalletSignatureType::Eoa
            })
        })
}

fn polymarket_signature_type_from_env(
    has_funder: bool,
) -> Result<WalletSignatureType, ExecutionError> {
    let value = first_env_value(&["POLY_SIGNATURE_TYPE", "POLYMARKET_SIGNATURE_TYPE"]);
    wallet_signature_type(value.as_deref(), has_funder)
}

fn is_auth_recovery_error(error: &ExecutionError) -> bool {
    match error {
        ExecutionError::Transport(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("401")
                || message.contains("unauthorized")
                || message.contains("invalid api key")
                || message.contains("expired")
        }
        ExecutionError::Configuration(_) | ExecutionError::Validation(_) => false,
    }
}

fn unique_token_ids(tracked_orders: &[TrackedOrder]) -> Result<Vec<U256>, ExecutionError> {
    let mut token_ids = Vec::new();

    for tracked_order in tracked_orders {
        let asset_id = U256::from_str(&tracked_order.token_id).map_err(|err| {
            ExecutionError::Validation(format!(
                "invalid token_id `{}`: {err}",
                tracked_order.token_id
            ))
        })?;
        if !token_ids.contains(&asset_id) {
            token_ids.push(asset_id);
        }
    }

    Ok(token_ids)
}

async fn load_trades_for_token(
    client: Client<Authenticated<Normal>>,
    token_id: U256,
) -> Result<Vec<TradeResponse>, ExecutionError> {
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| {
            duration
                .as_secs()
                .saturating_sub(TRADE_RECONCILE_LOOKBACK_SECS) as i64
        })
        .unwrap_or(0);
    let request = TradesRequest::builder()
        .asset_id(token_id)
        .after(after)
        .build();
    let mut next_cursor = None;
    let mut trades = Vec::new();

    loop {
        let page = client
            .trades(&request, next_cursor.clone())
            .await
            .map_err(|err| ExecutionError::Transport(format!("load trades: {err}")))?;
        trades.extend(page.data);

        if page.next_cursor.is_empty() || page.next_cursor == TERMINAL_CURSOR {
            break;
        }
        next_cursor = Some(page.next_cursor);
    }

    Ok(trades)
}

async fn polymarket_fee_schedule(
    client: &Client<Authenticated<Normal>>,
    cache: &RwLock<HashMap<U256, (FeeSchedule, Instant)>>,
    token_id: U256,
    condition_id: B256,
) -> Result<FeeSchedule, ExecutionError> {
    if let Some(schedule) = cache
        .read()
        .map_err(|_| ExecutionError::Transport("fee schedule cache poisoned".to_string()))?
        .get(&token_id)
        .filter(|(_, loaded_at)| loaded_at.elapsed() < FEE_SCHEDULE_TTL)
        .map(|(schedule, _)| *schedule)
    {
        return Ok(schedule);
    }

    let market = client
        .clob_market_info(&condition_id.to_string())
        .await
        .map_err(|err| ExecutionError::Transport(format!("load V2 market fee metadata: {err}")))?;
    if !market
        .tokens
        .iter()
        .flatten()
        .any(|token| token.token_id == token_id)
    {
        return Err(ExecutionError::Validation(format!(
            "market fee metadata for condition `{condition_id}` does not include token `{token_id}`"
        )));
    }
    let schedule = market.fee_details.as_ref().map_or_else(
        || FeeSchedule::polymarket_v2(Decimal::ZERO, 0, true),
        |fees| FeeSchedule::polymarket_v2(fees.rate, fees.exponent, fees.taker_only),
    );

    cache
        .write()
        .map_err(|_| ExecutionError::Transport("fee schedule cache poisoned".to_string()))?
        .insert(token_id, (schedule, Instant::now()));
    Ok(schedule)
}

async fn reconcile_rest_fills(
    client: &Client<Authenticated<Normal>>,
    fee_schedules: &RwLock<HashMap<U256, (FeeSchedule, Instant)>>,
    tracked_orders: &[TrackedOrder],
) -> Result<Vec<FillRecord>, ExecutionError> {
    let token_ids = unique_token_ids(tracked_orders)?;
    let mut trades = Vec::new();

    for chunk in token_ids.chunks(MAX_CONCURRENT_TRADE_RECONCILE_REQUESTS) {
        let mut tasks = tokio::task::JoinSet::new();
        for token_id in chunk {
            let client = client.clone();
            let token_id = *token_id;
            tasks.spawn(async move { load_trades_for_token(client, token_id).await });
        }
        while let Some(task) = tasks.join_next().await {
            let mut token_trades = task.map_err(|err| {
                ExecutionError::Transport(format!("join trade reconciliation task: {err}"))
            })??;
            trades.append(&mut token_trades);
        }
    }

    let mut fills = Vec::new();
    for trade in &trades {
        if !matches!(trade.status, TradeStatusType::Confirmed) {
            continue;
        }
        if !tracked_orders
            .iter()
            .any(|tracked_order| tracked_trade_matches(tracked_order, trade))
        {
            continue;
        }
        let fee_schedule =
            polymarket_fee_schedule(client, fee_schedules, trade.asset_id, trade.market).await?;
        for tracked_order in tracked_orders {
            if let Some(fill) = tracked_trade_fill(tracked_order, trade, fee_schedule) {
                fills.push(fill);
            }
        }
    }
    Ok(fills)
}

async fn load_order_observation(
    client: Client<Authenticated<Normal>>,
    tracked_order: TrackedOrder,
) -> Result<Option<OrderObservation>, ExecutionError> {
    let order = client
        .order(&tracked_order.venue_order_id)
        .await
        .map_err(|err| {
            ExecutionError::Transport(format!(
                "load order `{}`: {err}",
                tracked_order.venue_order_id
            ))
        })?;
    tracked_rest_order_observation(&tracked_order, &order)
}

async fn reconcile_rest_order_observations(
    client: &Client<Authenticated<Normal>>,
    tracked_orders: &[TrackedOrder],
) -> Result<Vec<OrderObservation>, ExecutionError> {
    let mut observations = Vec::new();
    for chunk in tracked_orders.chunks(MAX_CONCURRENT_TRADE_RECONCILE_REQUESTS) {
        let mut tasks = tokio::task::JoinSet::new();
        for tracked_order in chunk {
            tasks.spawn(load_order_observation(
                client.clone(),
                tracked_order.clone(),
            ));
        }
        while let Some(task) = tasks.join_next().await {
            if let Some(observation) = task.map_err(|err| {
                ExecutionError::Transport(format!("join order reconciliation task: {err}"))
            })?? {
                observations.push(observation);
            }
        }
    }
    Ok(observations)
}

async fn reconcile_rest_updates(
    client: &Client<Authenticated<Normal>>,
    fee_schedules: &RwLock<HashMap<U256, (FeeSchedule, Instant)>>,
    tracked_orders: &[TrackedOrder],
) -> Result<ReconcileBatch, ExecutionError> {
    let (fills, order_observations) = tokio::try_join!(
        reconcile_rest_fills(client, fee_schedules, tracked_orders),
        reconcile_rest_order_observations(client, tracked_orders),
    )?;
    Ok(ReconcileBatch {
        fills,
        order_observations,
    })
}

async fn sell_quantity_capped_to_balance(
    client: &Client<Authenticated<Normal>>,
    token_id: U256,
    requested_quantity: Decimal,
) -> Result<Decimal, ExecutionError> {
    let response = client
        .balance_allowance(
            BalanceAllowanceRequest::builder()
                .asset_type(AssetType::Conditional)
                .token_id(token_id)
                .build(),
        )
        .await
        .map_err(|err| {
            ExecutionError::Transport(format!("load conditional token balance: {err}"))
        })?;

    cap_sell_quantity_to_balance(
        requested_quantity,
        conditional_token_balance_to_shares(response.balance),
    )
}

fn cap_sell_quantity_to_balance(
    requested_quantity: Decimal,
    available_balance: Decimal,
) -> Result<Decimal, ExecutionError> {
    let requested = normalize_market_order_quantity(requested_quantity.max(Decimal::ZERO));
    let available = normalize_market_order_quantity(available_balance.max(Decimal::ZERO));
    let effective = requested.min(available);

    if effective <= Decimal::ZERO {
        return Err(ExecutionError::Validation(format!(
            "sell rejected before submit: no sellable conditional-token balance; requested={requested}, available={available}"
        )));
    }

    Ok(effective)
}

fn conditional_token_balance_to_shares(balance: Decimal) -> Decimal {
    let balance = balance.max(Decimal::ZERO);
    if balance.scale() == 0 {
        balance / Decimal::from(10_u64.pow(CONDITIONAL_TOKEN_DECIMALS))
    } else {
        balance
    }
}

fn pusd_to_raw(value: Decimal) -> Result<U256, ExecutionError> {
    if value <= Decimal::ZERO {
        return Err(ExecutionError::Validation(
            "required pUSD must be greater than zero".to_string(),
        ));
    }
    let raw = value
        .checked_mul(Decimal::from(10_u64.pow(CONDITIONAL_TOKEN_DECIMALS)))
        .ok_or_else(|| ExecutionError::Validation("required pUSD is out of range".to_string()))?;
    if !raw.fract().is_zero() {
        return Err(ExecutionError::Validation(
            "required pUSD supports at most 6 decimal places".to_string(),
        ));
    }
    U256::from_str(&raw.trunc().to_string()).map_err(|error| {
        ExecutionError::Validation(format!("required pUSD is out of range: {error}"))
    })
}

fn collateral_balance_to_raw(balance: Decimal) -> Result<U256, ExecutionError> {
    if balance < Decimal::ZERO {
        return Err(ExecutionError::Transport(
            "venue returned a negative collateral balance".to_string(),
        ));
    }
    if !balance.fract().is_zero() {
        return Err(ExecutionError::Transport(format!(
            "venue returned a fractional raw collateral balance: {balance}"
        )));
    }
    U256::from_str(&balance.trunc().to_string()).map_err(|error| {
        ExecutionError::Transport(format!("invalid raw collateral balance: {error}"))
    })
}

fn raw_balance_to_pusd(balance: U256) -> Result<Decimal, ExecutionError> {
    let raw = Decimal::from_str(&balance.to_string()).map_err(|error| {
        ExecutionError::Transport(format!("raw collateral balance is out of range: {error}"))
    })?;
    Ok(raw / Decimal::from(10_u64.pow(CONDITIONAL_TOKEN_DECIMALS)))
}

fn ensure_account_readiness(
    geoblocked: bool,
    closed_only: bool,
    balance_raw: U256,
    standard_allowance: &str,
    neg_risk_allowance: &str,
    required_raw: U256,
) -> Result<(), ExecutionError> {
    if geoblocked {
        return Err(ExecutionError::Validation(
            "Polymarket trading is geoblocked from this host".to_string(),
        ));
    }
    if closed_only {
        return Err(ExecutionError::Validation(
            "Polymarket account is in closed-only mode".to_string(),
        ));
    }
    if balance_raw < required_raw {
        return Err(ExecutionError::Validation(format!(
            "insufficient pUSD balance: required_raw={required_raw}, available_raw={balance_raw}"
        )));
    }
    for (label, allowance) in [
        ("standard V2", standard_allowance),
        ("neg-risk V2", neg_risk_allowance),
    ] {
        let allowance = U256::from_str(allowance).map_err(|error| {
            ExecutionError::Transport(format!("invalid {label} allowance: {error}"))
        })?;
        if allowance < required_raw {
            return Err(ExecutionError::Validation(format!(
                "insufficient {label} allowance: required_raw={required_raw}, available_raw={allowance}"
            )));
        }
    }
    Ok(())
}

fn normalize_aggressive_price(
    limit_price: Decimal,
    side: TradeSide,
    aggressive_ticks: u8,
    tick_size: Decimal,
) -> Decimal {
    let rounded_limit = (limit_price / tick_size).round() * tick_size;
    if aggressive_ticks == 0 {
        rounded_limit
    } else {
        let offset = tick_size * Decimal::from(aggressive_ticks);
        match side {
            TradeSide::Buy => (rounded_limit + offset).min(dec!(0.99)),
            TradeSide::Sell => (rounded_limit - offset).max(dec!(0.01)),
        }
    }
}

fn normalize_order_quantity(quantity: Decimal) -> Decimal {
    quantity.trunc_with_scale(2)
}

fn normalize_market_order_quantity(quantity: Decimal) -> Decimal {
    quantity.trunc_with_scale(4)
}

fn normalize_execution_amount(
    quantity: Decimal,
    _limit_price: Decimal,
    side: Side,
) -> Result<Amount, polymarket_client_sdk::error::Error> {
    let shares = normalize_order_quantity(quantity);
    match side {
        Side::Buy => Amount::shares(shares),
        Side::Sell => Amount::shares(shares),
        _ => unreachable!("invalid Polymarket side"),
    }
}

fn tracked_trade_fill(
    tracked_order: &TrackedOrder,
    trade: &TradeResponse,
    fee_schedule: FeeSchedule,
) -> Option<FillRecord> {
    if !matches!(trade.status, TradeStatusType::Confirmed) {
        return None;
    }
    let tracked_token_id = U256::from_str(&tracked_order.token_id).ok()?;
    if trade.taker_order_id == tracked_order.venue_order_id {
        if trade.asset_id != tracked_token_id {
            return None;
        }
        if trade_side(trade.side)? != tracked_order.side {
            return None;
        }
        return Some(FillRecord {
            fill_id: tracked_fill_id(tracked_order, &trade.id),
            order_id: tracked_order.order_id.clone(),
            token_id: tracked_order.token_id.clone(),
            side: tracked_order.side,
            quantity: trade.size,
            price: trade.price,
            fee: fee_schedule.calculate(trade.size, trade.price, LiquidityRole::Taker),
            timestamp: trade.match_time,
        });
    }

    trade
        .maker_orders
        .iter()
        .find(|maker_order| maker_order.order_id == tracked_order.venue_order_id)
        .and_then(|maker_order| {
            tracked_maker_fill(
                tracked_order,
                trade,
                maker_order,
                tracked_token_id,
                fee_schedule,
            )
        })
}

fn tracked_trade_matches(tracked_order: &TrackedOrder, trade: &TradeResponse) -> bool {
    if !matches!(trade.status, TradeStatusType::Confirmed) {
        return false;
    }
    let Ok(tracked_token_id) = U256::from_str(&tracked_order.token_id) else {
        return false;
    };
    if trade.taker_order_id == tracked_order.venue_order_id {
        return trade.asset_id == tracked_token_id
            && trade_side(trade.side) == Some(tracked_order.side);
    }
    trade.maker_orders.iter().any(|maker_order| {
        maker_order.order_id == tracked_order.venue_order_id
            && maker_order.asset_id == tracked_token_id
            && trade_side(maker_order.side) == Some(tracked_order.side)
    })
}

fn tracked_maker_fill(
    tracked_order: &TrackedOrder,
    trade: &TradeResponse,
    maker_order: &MakerOrder,
    tracked_token_id: U256,
    fee_schedule: FeeSchedule,
) -> Option<FillRecord> {
    if maker_order.asset_id != tracked_token_id {
        return None;
    }
    if trade_side(maker_order.side)? != tracked_order.side {
        return None;
    }
    Some(FillRecord {
        fill_id: tracked_fill_id(tracked_order, &trade.id),
        order_id: tracked_order.order_id.clone(),
        token_id: tracked_order.token_id.clone(),
        side: tracked_order.side,
        quantity: maker_order.matched_amount,
        price: maker_order.price,
        fee: fee_schedule.calculate(
            maker_order.matched_amount,
            maker_order.price,
            LiquidityRole::Maker,
        ),
        timestamp: trade.match_time,
    })
}

fn user_trade_timestamp(trade: &TradeMessage) -> Option<DateTime<Utc>> {
    trade
        .matchtime
        .or(trade.timestamp)
        .or(trade.last_update)
        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
}

fn maker_side_from_taker(side: Side) -> Option<TradeSide> {
    match trade_side(side)? {
        TradeSide::Buy => Some(TradeSide::Sell),
        TradeSide::Sell => Some(TradeSide::Buy),
    }
}

fn tracked_user_trade_fill(
    tracked_order: &TrackedOrder,
    trade: &TradeMessage,
    fee_schedule: FeeSchedule,
) -> Option<FillRecord> {
    if !matches!(trade.status, TradeMessageStatus::Confirmed) {
        return None;
    }
    let tracked_token_id = U256::from_str(&tracked_order.token_id).ok()?;
    let timestamp = user_trade_timestamp(trade)?;

    if trade.taker_order_id.as_deref() == Some(tracked_order.venue_order_id.as_str()) {
        if trade.asset_id != tracked_token_id || trade_side(trade.side)? != tracked_order.side {
            return None;
        }
        return Some(FillRecord {
            fill_id: tracked_fill_id(tracked_order, &trade.id),
            order_id: tracked_order.order_id.clone(),
            token_id: tracked_order.token_id.clone(),
            side: tracked_order.side,
            quantity: trade.size,
            price: trade.price,
            fee: fee_schedule.calculate(trade.size, trade.price, LiquidityRole::Taker),
            timestamp,
        });
    }

    let maker_order = trade
        .maker_orders
        .iter()
        .find(|maker_order| maker_order.order_id == tracked_order.venue_order_id)?;
    if maker_order.asset_id != tracked_token_id
        || maker_side_from_taker(trade.side)? != tracked_order.side
    {
        return None;
    }
    Some(FillRecord {
        fill_id: tracked_fill_id(tracked_order, &trade.id),
        order_id: tracked_order.order_id.clone(),
        token_id: tracked_order.token_id.clone(),
        side: tracked_order.side,
        quantity: maker_order.matched_amount,
        price: maker_order.price,
        fee: fee_schedule.calculate(
            maker_order.matched_amount,
            maker_order.price,
            LiquidityRole::Maker,
        ),
        timestamp,
    })
}

fn tracked_order_observation(
    tracked_order: &TrackedOrder,
    order: &OrderMessage,
) -> Option<OrderObservation> {
    let tracked_token_id = U256::from_str(&tracked_order.token_id).ok()?;
    if order.id != tracked_order.venue_order_id
        || order.asset_id != tracked_token_id
        || trade_side(order.side)? != tracked_order.side
    {
        return None;
    }

    if matches!(order.msg_type, Some(OrderMessageType::Cancellation))
        || matches!(order.status, Some(OrderStatusType::Canceled))
    {
        return Some(OrderObservation::Canceled {
            order_id: tracked_order.order_id.clone(),
        });
    }
    if matches!(order.msg_type, Some(OrderMessageType::Placement))
        || matches!(
            order.status,
            Some(OrderStatusType::Live | OrderStatusType::Unmatched | OrderStatusType::Matched)
        )
    {
        return Some(OrderObservation::Acknowledged {
            order_id: tracked_order.order_id.clone(),
            venue_order_id: tracked_order.venue_order_id.clone(),
        });
    }
    None
}

fn tracked_rest_order_observation(
    tracked_order: &TrackedOrder,
    order: &OpenOrderResponse,
) -> Result<Option<OrderObservation>, ExecutionError> {
    let tracked_token_id = U256::from_str(&tracked_order.token_id).map_err(|err| {
        ExecutionError::Validation(format!(
            "invalid tracked token `{}`: {err}",
            tracked_order.token_id
        ))
    })?;
    if order.id != tracked_order.venue_order_id
        || order.asset_id != tracked_token_id
        || trade_side(order.side) != Some(tracked_order.side)
    {
        return Err(ExecutionError::Validation(format!(
            "venue order `{}` does not match tracked order `{}`",
            order.id, tracked_order.order_id
        )));
    }

    match &order.status {
        OrderStatusType::Live | OrderStatusType::Unmatched | OrderStatusType::Matched => {
            Ok(Some(OrderObservation::Acknowledged {
                order_id: tracked_order.order_id.clone(),
                venue_order_id: tracked_order.venue_order_id.clone(),
            }))
        }
        OrderStatusType::Canceled => Ok(Some(OrderObservation::Canceled {
            order_id: tracked_order.order_id.clone(),
        })),
        OrderStatusType::Delayed | OrderStatusType::Unknown(_) => {
            Err(ExecutionError::Transport(format!(
                "venue order `{}` has unresolved status `{}`",
                tracked_order.venue_order_id, order.status
            )))
        }
        _ => Err(ExecutionError::Transport(format!(
            "venue order `{}` has unsupported status `{}`",
            tracked_order.venue_order_id, order.status
        ))),
    }
}

fn tracked_user_trade_matches(tracked_order: &TrackedOrder, trade: &TradeMessage) -> bool {
    if !matches!(trade.status, TradeMessageStatus::Confirmed) {
        return false;
    }
    let Ok(tracked_token_id) = U256::from_str(&tracked_order.token_id) else {
        return false;
    };
    if trade.taker_order_id.as_deref() == Some(tracked_order.venue_order_id.as_str()) {
        return trade.asset_id == tracked_token_id
            && trade_side(trade.side) == Some(tracked_order.side);
    }
    trade.maker_orders.iter().any(|maker_order| {
        maker_order.order_id == tracked_order.venue_order_id
            && maker_order.asset_id == tracked_token_id
            && maker_side_from_taker(trade.side) == Some(tracked_order.side)
    })
}

async fn reconcile_user_events(
    client: &Client<Authenticated<Normal>>,
    fee_schedules: &RwLock<HashMap<U256, (FeeSchedule, Instant)>>,
    tracked_orders: &[TrackedOrder],
    events: Vec<BufferedUserEvent>,
) -> Result<ReconcileBatch, ExecutionError> {
    let mut batch = ReconcileBatch::default();
    for event in events {
        match event {
            BufferedUserEvent::Trade(trade) => {
                if !matches!(trade.status, TradeMessageStatus::Confirmed) {
                    continue;
                }
                if !tracked_orders
                    .iter()
                    .any(|tracked_order| tracked_user_trade_matches(tracked_order, &trade))
                {
                    continue;
                }
                let fee_schedule =
                    polymarket_fee_schedule(client, fee_schedules, trade.asset_id, trade.market)
                        .await?;
                for tracked_order in tracked_orders {
                    if let Some(fill) = tracked_user_trade_fill(tracked_order, &trade, fee_schedule)
                    {
                        batch.fills.push(fill);
                    }
                }
            }
            BufferedUserEvent::Order(order) => {
                for tracked_order in tracked_orders {
                    if let Some(observation) = tracked_order_observation(tracked_order, &order) {
                        if !batch.order_observations.contains(&observation) {
                            batch.order_observations.push(observation);
                        }
                    }
                }
            }
        }
    }
    Ok(batch)
}

fn deduplicate_fills(fills: &mut Vec<FillRecord>) {
    let mut seen = HashSet::with_capacity(fills.len());
    fills.retain(|fill| seen.insert(fill.fill_id.clone()));
}

fn deduplicate_order_observations(observations: &mut Vec<OrderObservation>) {
    let mut seen = HashSet::with_capacity(observations.len());
    observations.retain(|observation| seen.insert(observation.clone()));
}

fn tracked_fill_id(tracked_order: &TrackedOrder, trade_id: &str) -> String {
    format!("{trade_id}:{}", tracked_order.order_id)
}

fn trade_side(side: Side) -> Option<TradeSide> {
    match side {
        Side::Buy => Some(TradeSide::Buy),
        Side::Sell => Some(TradeSide::Sell),
        _ => None,
    }
}

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

#[cfg(test)]
mod tests {
    use super::{
        cap_sell_quantity_to_balance, collateral_balance_to_raw,
        conditional_token_balance_to_shares, ensure_account_readiness, execution_price_override,
        normalize_aggressive_price, normalize_execution_amount, normalize_market_order_quantity,
        normalize_order_quantity, polymarket_execution_principal, raw_balance_to_pusd,
        tracked_rest_order_observation, tracked_trade_fill, tracked_user_trade_fill,
        tracked_user_trade_matches, trade_side, unique_token_ids, wallet_signature_type,
        BufferedUserEvent, CancellationOutcome, CancellationRequest, ExecutionError,
        ExecutionOutcome, ExecutionRequest, LiveExecutionGateway, OrderExecutionType,
        OrderObservation, PolymarketExecutionConfig, PolymarketExecutionGateway, ReplaceOutcome,
        ReplaceRequest, StaticExecutionGateway, TrackedOrder, UserEventStream, WalletSignatureType,
    };
    use chrono::Utc;
    use ploy_trading::{FillRecord, TradeSide};
    use polymarket_client_sdk::auth::ApiKey;
    use polymarket_client_sdk::clob::types::response::{
        MakerOrder, OpenOrderResponse, TradeResponse,
    };
    use polymarket_client_sdk::clob::types::{
        OrderStatusType, OrderType, Side, TradeStatusType, TraderSide,
    };
    use polymarket_client_sdk::clob::ws::types::response::TradeMessage;
    use polymarket_client_sdk::types::{Address, B256, U256};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use secrecy::SecretString;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;

    #[test]
    fn static_gateway_returns_acknowledged_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1");
        let outcome = gateway
            .submit(&ExecutionRequest {
                order_id: "order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.55)),
                order_type: OrderExecutionType::GTC,
                aggressive_ticks: 0,
            })
            .expect("ack outcome");

        assert_eq!(
            outcome,
            ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-order-1".to_string()
            }
        );
    }

    #[test]
    fn execution_principal_is_derived_from_signer_or_funder() {
        let eoa = polymarket_execution_principal(&PolymarketExecutionConfig {
            private_key: Some(SecretString::from(format!("0x{:064x}", 1))),
            signature_type: WalletSignatureType::Eoa,
            ..PolymarketExecutionConfig::default()
        })
        .expect("eoa principal");
        assert_eq!(eoa, "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf");

        let proxy = polymarket_execution_principal(&PolymarketExecutionConfig {
            private_key: Some(SecretString::from(
                "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string(),
            )),
            signature_type: WalletSignatureType::Proxy,
            funder: Some("0x365f0ca36ae1f641e02fe3b7743673da42a13a70".to_string()),
            ..PolymarketExecutionConfig::default()
        })
        .expect("proxy principal");
        assert_eq!(proxy, "0x365f0ca36ae1f641e02fe3b7743673da42a13a70");

        let mismatched_proxy = polymarket_execution_principal(&PolymarketExecutionConfig {
            private_key: Some(SecretString::from(
                "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string(),
            )),
            signature_type: WalletSignatureType::Proxy,
            funder: Some("0x1111111111111111111111111111111111111111".to_string()),
            ..PolymarketExecutionConfig::default()
        });
        assert!(mismatched_proxy.is_err());
    }

    #[test]
    fn polymarket_gateway_rejects_missing_limit_price_before_network() {
        let gateway = PolymarketExecutionGateway::new(PolymarketExecutionConfig {
            host: "https://clob.polymarket.com".to_string(),
            private_key: None,
            use_server_time: true,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
        });

        let error = gateway
            .submit(&ExecutionRequest {
                order_id: "order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: None,
                order_type: OrderExecutionType::GTC,
                aggressive_ticks: 0,
            })
            .expect_err("limit price should be required");

        assert_eq!(
            error,
            ExecutionError::Validation(
                "live Polymarket execution currently requires a limit price".to_string()
            )
        );
    }

    #[test]
    fn normalize_aggressive_price_rounds_to_tick_size_before_offset() {
        assert_eq!(
            normalize_aggressive_price(dec!(0.607925), TradeSide::Buy, 0, dec!(0.01)),
            dec!(0.61)
        );
        assert_eq!(
            normalize_aggressive_price(dec!(0.607925), TradeSide::Buy, 2, dec!(0.01)),
            dec!(0.63)
        );
        assert_eq!(
            normalize_aggressive_price(dec!(0.607925), TradeSide::Sell, 2, dec!(0.01)),
            dec!(0.59)
        );
    }

    #[test]
    fn normalize_order_quantity_truncates_to_lot_size_scale() {
        assert_eq!(normalize_order_quantity(dec!(24.467825)), dec!(24.46));
        assert_eq!(normalize_order_quantity(dec!(17.663163)), dec!(17.66));
    }

    #[test]
    fn normalize_market_order_quantity_truncates_to_market_taker_scale() {
        assert_eq!(
            normalize_market_order_quantity(dec!(24.467825)),
            dec!(24.4678)
        );
        assert_eq!(
            normalize_market_order_quantity(dec!(17.663163)),
            dec!(17.6631)
        );
    }

    #[test]
    fn normalize_execution_amount_uses_shares_for_immediate_orders() {
        let buy = normalize_execution_amount(dec!(24.467825), dec!(0.61305), Side::Buy)
            .expect("buy amount");
        let sell = normalize_execution_amount(dec!(24.467825), dec!(0.61305), Side::Sell)
            .expect("sell amount");

        assert!(buy.is_shares());
        assert_eq!(buy.as_inner(), dec!(24.46));
        assert!(sell.is_shares());
        assert_eq!(sell.as_inner(), dec!(24.46));
    }

    #[test]
    fn sell_quantity_is_capped_to_conditional_token_balance() {
        let capped =
            cap_sell_quantity_to_balance(dec!(5), dec!(4.881280)).expect("available balance");

        assert_eq!(capped, dec!(4.8812));
    }

    #[test]
    fn raw_conditional_token_balance_units_are_scaled_before_capping_sell() {
        let available = conditional_token_balance_to_shares(dec!(28291106));
        let capped = cap_sell_quantity_to_balance(dec!(29.32), available)
            .expect("raw balance should cap to sellable shares");

        assert_eq!(available, dec!(28.291106));
        assert_eq!(capped, dec!(28.2911));
    }

    #[test]
    fn already_scaled_conditional_token_balances_are_preserved() {
        assert_eq!(
            conditional_token_balance_to_shares(dec!(4.881280)),
            dec!(4.881280)
        );
    }

    #[test]
    fn collateral_readiness_balance_is_always_raw_base_units() {
        assert_eq!(
            collateral_balance_to_raw(dec!(5000000)).expect("raw balance"),
            U256::from(5_000_000_u64)
        );
        assert_eq!(
            collateral_balance_to_raw(dec!(5000000.0)).expect("raw balance with scale"),
            U256::from(5_000_000_u64)
        );
        assert!(collateral_balance_to_raw(dec!(5000000.5)).is_err());
        assert_eq!(
            raw_balance_to_pusd(U256::from(5_000_000_u64)).expect("display balance"),
            dec!(5)
        );
    }

    #[test]
    fn sell_quantity_rejects_when_conditional_token_balance_is_zero() {
        let error = cap_sell_quantity_to_balance(dec!(5), Decimal::ZERO)
            .expect_err("zero balance should reject");

        assert!(matches!(error, ExecutionError::Validation(_)));
        assert!(error
            .to_string()
            .contains("no sellable conditional-token balance"));
    }

    #[test]
    fn unknown_venue_trade_side_is_not_mapped_to_buy() {
        assert_eq!(trade_side(Side::Unknown), None);
    }

    #[test]
    fn signature_type_defaults_to_proxy_when_funder_is_present() {
        assert_eq!(
            wallet_signature_type(None, true).expect("default"),
            WalletSignatureType::Proxy
        );
        assert_eq!(
            wallet_signature_type(None, false).expect("default"),
            WalletSignatureType::Eoa
        );
        assert_eq!(
            wallet_signature_type(Some("poly1271"), true).expect("deposit wallet"),
            WalletSignatureType::Poly1271
        );
        assert!(wallet_signature_type(Some("typo"), false).is_err());
    }

    #[test]
    fn account_readiness_fails_closed_on_every_trading_gate() {
        let required = U256::from(5_000_000_u64);
        let enough = U256::from(10_000_000_u64);
        let allowance = "10000000";

        assert!(
            ensure_account_readiness(false, false, enough, allowance, allowance, required).is_ok()
        );
        assert!(
            ensure_account_readiness(true, false, enough, allowance, allowance, required).is_err()
        );
        assert!(
            ensure_account_readiness(false, true, enough, allowance, allowance, required).is_err()
        );
        assert!(
            ensure_account_readiness(false, false, U256::ZERO, allowance, allowance, required)
                .is_err()
        );
        assert!(ensure_account_readiness(false, false, enough, "0", allowance, required).is_err());
        assert!(ensure_account_readiness(false, false, enough, allowance, "0", required).is_err());
    }

    #[test]
    fn readiness_uses_a_dedicated_client_with_heartbeats_stopped() {
        let source = include_str!("lib.rs");
        let readiness = &source[source.find("pub fn account_readiness").unwrap()..];
        let readiness = &readiness[..readiness.find("impl LiveExecutionGateway").unwrap()];

        assert!(readiness.contains("derive_api_key(&signer, None)"));
        assert!(readiness.contains(".credentials(credentials)"));
        assert!(readiness.contains("stop_heartbeats().await"));
        assert!(readiness.contains("heartbeats_active()"));
        assert!(!readiness.contains("create_or_derive_api_key"));
        assert!(!readiness.contains("create_api_key"));
        assert!(
            readiness.find("tokio::time::timeout").unwrap()
                < readiness.find("derive_api_key(&signer, None)").unwrap()
        );
        assert!(
            readiness.find("stop_heartbeats().await").unwrap()
                < readiness.find("client.version().await").unwrap()
        );
    }

    #[test]
    fn immediate_execution_keeps_a_hard_price_bound() {
        assert_eq!(
            execution_price_override(OrderExecutionType::GTC, TradeSide::Buy, dec!(0.417075), 2),
            Some(dec!(0.44))
        );
        assert_eq!(
            execution_price_override(OrderExecutionType::FAK, TradeSide::Buy, dec!(0.417075), 2),
            Some(dec!(0.417075))
        );
        assert_eq!(
            execution_price_override(OrderExecutionType::FOK, TradeSide::Buy, dec!(0.417075), 2),
            Some(dec!(0.417075))
        );
    }

    #[test]
    fn static_gateway_replays_reconciled_fills() {
        let fill = FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.55),
            fee: dec!(0.01),
            timestamp: Utc::now(),
        };
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1")
            .with_reconciled_fills(vec![fill.clone()]);

        let fills = gateway
            .reconcile_fills(&[TrackedOrder {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
            }])
            .expect("fills");

        assert_eq!(fills, vec![fill]);
    }

    #[test]
    fn static_gateway_replays_cancel_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1").with_cancel_result(Ok(
            CancellationOutcome::Rejected {
                reason: "already filled".to_string(),
            },
        ));

        let outcome = gateway
            .cancel(&CancellationRequest {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
            })
            .expect("cancel outcome");

        assert_eq!(
            outcome,
            CancellationOutcome::Rejected {
                reason: "already filled".to_string(),
            }
        );
    }

    #[test]
    fn static_gateway_replays_replace_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1").with_replace_result(
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-order-2".to_string(),
            }),
        );

        let outcome = gateway
            .replace(&ReplaceRequest {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.57)),
            })
            .expect("replace outcome");

        assert_eq!(
            outcome,
            ReplaceOutcome::Replaced {
                venue_order_id: "venue-order-2".to_string(),
            }
        );
    }

    #[test]
    fn tracked_fill_ids_are_scoped_by_tracked_order() {
        let trade = TradeResponse::builder()
            .id("trade-1")
            .taker_order_id("venue-order-a")
            .market(B256::ZERO)
            .asset_id(U256::from(1_u64))
            .side(polymarket_client_sdk::clob::types::Side::Buy)
            .size(dec!(2))
            .fee_rate_bps(dec!(5))
            .price(dec!(0.55))
            .status(TradeStatusType::Confirmed)
            .match_time(Utc::now())
            .last_update(Utc::now())
            .outcome("YES")
            .bucket_index(0)
            .owner(ApiKey::nil())
            .maker_address(
                Address::from_str("0x0000000000000000000000000000000000000001")
                    .expect("maker address"),
            )
            .maker_orders(vec![MakerOrder::builder()
                .order_id("venue-order-b")
                .owner(ApiKey::nil())
                .maker_address(
                    Address::from_str("0x0000000000000000000000000000000000000002")
                        .expect("second maker address"),
                )
                .matched_amount(dec!(2))
                .price(dec!(0.56))
                .fee_rate_bps(dec!(4))
                .asset_id(U256::from(1_u64))
                .outcome("YES")
                .side(polymarket_client_sdk::clob::types::Side::Sell)
                .build()])
            .transaction_hash(B256::ZERO)
            .trader_side(TraderSide::Taker)
            .build();

        let taker_fill = tracked_trade_fill(
            &TrackedOrder {
                order_id: "order-a".to_string(),
                venue_order_id: "venue-order-a".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
            },
            &trade,
            ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
        )
        .expect("taker fill");
        let maker_fill = tracked_trade_fill(
            &TrackedOrder {
                order_id: "order-b".to_string(),
                venue_order_id: "venue-order-b".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Sell,
            },
            &trade,
            ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
        )
        .expect("maker fill");

        assert_eq!(taker_fill.fill_id, "trade-1:order-a");
        assert_eq!(maker_fill.fill_id, "trade-1:order-b");
        assert_ne!(taker_fill.fill_id, maker_fill.fill_id);
        assert_eq!(taker_fill.fee, dec!(0.03465));
        assert_eq!(maker_fill.fee, dec!(0));
        assert!(tracked_trade_fill(
            &TrackedOrder {
                order_id: "order-side-mismatch".to_string(),
                venue_order_id: "venue-order-a".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Sell,
            },
            &trade,
            ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
        )
        .is_none());
    }

    #[test]
    fn reconciliation_ignores_non_confirmed_trade_statuses() {
        let tracked = TrackedOrder {
            order_id: "order-a".to_string(),
            venue_order_id: "venue-order-a".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Buy,
        };

        for status in [
            TradeStatusType::Matched,
            TradeStatusType::Mined,
            TradeStatusType::Retrying,
            TradeStatusType::Failed,
        ] {
            let trade = TradeResponse::builder()
                .id("trade-status")
                .taker_order_id("venue-order-a")
                .market(B256::ZERO)
                .asset_id(U256::from(1_u64))
                .side(polymarket_client_sdk::clob::types::Side::Buy)
                .size(dec!(2))
                .fee_rate_bps(dec!(5))
                .price(dec!(0.55))
                .status(status)
                .match_time(Utc::now())
                .last_update(Utc::now())
                .outcome("YES")
                .bucket_index(0)
                .owner(ApiKey::nil())
                .maker_address(
                    Address::from_str("0x0000000000000000000000000000000000000001")
                        .expect("maker address"),
                )
                .maker_orders(Vec::new())
                .transaction_hash(B256::ZERO)
                .trader_side(TraderSide::Taker)
                .build();

            assert!(tracked_trade_fill(
                &tracked,
                &trade,
                ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true),
            )
            .is_none());
        }
    }

    #[test]
    fn unique_token_ids_deduplicates_reconcile_requests() {
        let token_ids = unique_token_ids(&[
            TrackedOrder {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-1".to_string(),
                token_id: "10".to_string(),
                side: TradeSide::Buy,
            },
            TrackedOrder {
                order_id: "order-2".to_string(),
                venue_order_id: "venue-2".to_string(),
                token_id: "10".to_string(),
                side: TradeSide::Sell,
            },
            TrackedOrder {
                order_id: "order-3".to_string(),
                venue_order_id: "venue-3".to_string(),
                token_id: "11".to_string(),
                side: TradeSide::Buy,
            },
        ])
        .expect("valid token ids");

        assert_eq!(token_ids, vec![U256::from(10), U256::from(11)]);
    }

    fn user_trade(status: &str) -> TradeMessage {
        serde_json::from_str(&format!(
            r#"{{
                "asset_id":"1",
                "event_type":"trade",
                "id":"trade-ws",
                "last_update":"1672290701",
                "maker_orders":[{{
                    "asset_id":"1",
                    "matched_amount":"2",
                    "order_id":"venue-maker",
                    "outcome":"YES",
                    "owner":"00000000-0000-0000-0000-000000000000",
                    "price":"0.56"
                }}],
                "market":"0x0000000000000000000000000000000000000000000000000000000000000000",
                "match_time":"1672290701",
                "outcome":"YES",
                "owner":"00000000-0000-0000-0000-000000000000",
                "price":"0.55",
                "side":"BUY",
                "size":"2",
                "status":"{status}",
                "taker_order_id":"venue-taker",
                "timestamp":"1672290701",
                "trade_owner":"00000000-0000-0000-0000-000000000000",
                "type":"TRADE"
            }}"#
        ))
        .expect("valid user trade fixture")
    }

    #[test]
    fn user_trade_reconciliation_is_confirmed_only_and_preserves_liquidity_role() {
        let taker = TrackedOrder {
            order_id: "order-taker".to_string(),
            venue_order_id: "venue-taker".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Buy,
        };
        let maker = TrackedOrder {
            order_id: "order-maker".to_string(),
            venue_order_id: "venue-maker".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Sell,
        };
        let fee_schedule = ploy_market_contracts::FeeSchedule::polymarket_v2(dec!(0.07), 1, true);

        for status in ["MATCHED", "MINED", "RETRYING", "FAILED"] {
            assert!(tracked_user_trade_fill(&taker, &user_trade(status), fee_schedule).is_none());
        }

        let confirmed = user_trade("CONFIRMED");
        let taker_fill =
            tracked_user_trade_fill(&taker, &confirmed, fee_schedule).expect("taker fill");
        let maker_fill =
            tracked_user_trade_fill(&maker, &confirmed, fee_schedule).expect("maker fill");
        assert_eq!(taker_fill.fill_id, "trade-ws:order-taker");
        assert_eq!(maker_fill.fill_id, "trade-ws:order-maker");
        assert_eq!(taker_fill.fee, dec!(0.03465));
        assert_eq!(maker_fill.fee, Decimal::ZERO);

        let wrong_maker_side = TrackedOrder {
            side: TradeSide::Buy,
            ..maker
        };
        assert!(tracked_user_trade_fill(&wrong_maker_side, &confirmed, fee_schedule).is_none());
        assert!(!tracked_user_trade_matches(&wrong_maker_side, &confirmed));
    }

    #[test]
    fn rest_order_recovery_validates_identity_and_maps_status() {
        let tracked = TrackedOrder {
            order_id: "order-1".to_string(),
            venue_order_id: "venue-1".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Buy,
        };
        let order = |status| {
            OpenOrderResponse::builder()
                .id("venue-1")
                .status(status)
                .owner(ApiKey::nil())
                .maker_address(Address::ZERO)
                .market(B256::ZERO)
                .asset_id(U256::from(1_u64))
                .side(Side::Buy)
                .original_size(dec!(2))
                .size_matched(dec!(1))
                .price(dec!(0.55))
                .associate_trades(Vec::new())
                .outcome("YES")
                .created_at(Utc::now())
                .expiration(Utc::now())
                .order_type(OrderType::GTC)
                .build()
        };

        assert_eq!(
            tracked_rest_order_observation(&tracked, &order(OrderStatusType::Matched))
                .expect("matched order"),
            Some(OrderObservation::Acknowledged {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-1".to_string(),
            })
        );
        assert_eq!(
            tracked_rest_order_observation(&tracked, &order(OrderStatusType::Canceled))
                .expect("canceled order"),
            Some(OrderObservation::Canceled {
                order_id: "order-1".to_string(),
            })
        );

        let mut wrong_token = order(OrderStatusType::Live);
        wrong_token.asset_id = U256::from(2_u64);
        assert!(tracked_rest_order_observation(&tracked, &wrong_token).is_err());
        assert!(
            tracked_rest_order_observation(&tracked, &order(OrderStatusType::Delayed)).is_err()
        );
    }

    #[test]
    fn user_event_gap_is_bounded_and_not_cleared_while_degraded() {
        let state = UserEventStream::new();
        state.gap_detected.store(false, Ordering::Release);
        let trade = user_trade("CONFIRMED");
        for _ in 0..=super::USER_EVENT_QUEUE_CAPACITY {
            state.enqueue(BufferedUserEvent::Trade(trade.clone()));
        }
        assert!(state.gap_detected.load(Ordering::Acquire));

        state.connected.store(false, Ordering::Release);
        assert!(state.begin_catch_up().expect("catch-up state"));
        state.finish_catch_up(true);
        assert!(state.gap_detected.load(Ordering::Acquire));

        state.connected.store(true, Ordering::Release);
        assert!(state.begin_catch_up().expect("catch-up state"));
        state.gap_detected.store(true, Ordering::Release);
        state.finish_catch_up(true);
        assert!(state.gap_detected.load(Ordering::Acquire));
    }
}
