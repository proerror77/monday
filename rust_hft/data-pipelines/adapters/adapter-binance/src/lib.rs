//! Binance 行情 adapter（實作 `ports::MarketStream`）
//! - 快照+增量/序號/checksum → 統一 MarketEvent
//! - WebSocket 實時流 + REST 快照初始化

use async_trait::async_trait;
use bytes::BytesMut;
use futures::StreamExt;
use hft_core::{
    now_micros, HftError, HftResult, InstrumentSpec, LatencyStage, LatencyTracker,
    LocalReceiveTimestamp, ProductType, Symbol,
};
use integration::WsMessageMetrics;
use ports::events::MarketSnapshot;
use ports::{
    BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketStream, TrackedMarketEvent,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

mod converter;
mod message_types;
mod rest;
mod websocket;

// Re-export for benchmarks and external use
pub use converter::MessageConverter;
pub use message_types::{BookTickerEvent, DepthSnapshot};
pub use rest::BinanceRestClient;
pub use websocket::BinanceWebSocket;

const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;
const DEFAULT_SYNC_BUFFER_CAPACITY: usize = 16_384;
const REST_SNAPSHOT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug)]
struct QueuedMarketEvent {
    generation: u64,
    event: TrackedMarketEvent,
}

struct ParsedTrackedMarketEvent {
    event: MarketEvent,
    tracker: LatencyTracker,
    previous_update_id: Option<u64>,
}

impl ParsedTrackedMarketEvent {
    fn into_tracked(self) -> TrackedMarketEvent {
        TrackedMarketEvent {
            event: self.event,
            tracker: self.tracker,
        }
    }
}

#[derive(Debug, Clone)]
struct StreamInvalidation {
    generation: u64,
    reason: String,
    error: Option<HftError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePublishError {
    Full,
    Closed,
}

fn publish_invalidation(
    generation: &AtomicU64,
    control_tx: &watch::Sender<Option<StreamInvalidation>>,
    reason: String,
    error: Option<HftError>,
) {
    let generation = generation.fetch_add(1, Ordering::AcqRel).saturating_add(1);
    control_tx.send_replace(Some(StreamInvalidation {
        generation,
        reason,
        error,
    }));
}

fn try_queue_market_event(
    tx: &mpsc::Sender<QueuedMarketEvent>,
    generation: &AtomicU64,
    event: TrackedMarketEvent,
) -> Result<(), QueuePublishError> {
    match tx.try_send(QueuedMarketEvent {
        generation: generation.load(Ordering::Acquire),
        event,
    }) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => Err(QueuePublishError::Full),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(QueuePublishError::Closed),
    }
}

fn multiplex_market_events(
    mut data_rx: mpsc::Receiver<QueuedMarketEvent>,
    mut control_rx: watch::Receiver<Option<StreamInvalidation>>,
    generation: Arc<AtomicU64>,
) -> BoxStream<TrackedMarketEvent> {
    Box::pin(async_stream::stream! {
        let mut control_open = true;
        loop {
            tokio::select! {
                biased;
                changed = control_rx.changed(), if control_open => {
                    if changed.is_err() {
                        control_open = false;
                        continue;
                    }
                    let invalidation = control_rx.borrow_and_update().clone();
                    let Some(invalidation) = invalidation else {
                        continue;
                    };
                    if invalidation.generation != generation.load(Ordering::Acquire) {
                        continue;
                    }
                    yield Ok(TrackedMarketEvent::new(MarketEvent::Disconnect {
                        reason: invalidation.reason,
                        source_venue: Some(hft_core::VenueId::BINANCE),
                        symbol: None,
                    }));
                    if let Some(error) = invalidation.error {
                        yield Err(error);
                    }
                }
                queued = data_rx.recv() => {
                    let Some(queued) = queued else {
                        break;
                    };
                    if queued.generation == generation.load(Ordering::Acquire) {
                        yield Ok(queued.event);
                    }
                }
            }
        }
    })
}

#[derive(Debug, Default)]
struct DepthSequenceTracker {
    last_update_ids: HashMap<Symbol, u64>,
    synchronized_usdm_updates: HashSet<Symbol>,
    usdm: bool,
}

impl DepthSequenceTracker {
    fn from_snapshots<'a>(
        snapshots: impl IntoIterator<Item = &'a MarketSnapshot>,
        usdm: bool,
    ) -> Self {
        Self {
            last_update_ids: snapshots
                .into_iter()
                .map(|snapshot| (snapshot.symbol.clone(), snapshot.sequence))
                .collect(),
            synchronized_usdm_updates: HashSet::new(),
            usdm,
        }
    }

    /// Returns `true` when the event must be forwarded and `false` for a stale depth event.
    fn validate_and_advance(
        &mut self,
        event: &MarketEvent,
        previous_update_id: Option<u64>,
    ) -> HftResult<bool> {
        let MarketEvent::Update(BookUpdate {
            symbol,
            first_sequence,
            sequence,
            ..
        }) = event
        else {
            return Ok(true);
        };
        let Some(previous) = self.last_update_ids.get(symbol).copied() else {
            return Err(HftError::Network(format!(
                "Binance depth update arrived before snapshot for {}",
                symbol.as_str()
            )));
        };
        let expected = previous.saturating_add(1);
        if *sequence < expected {
            return Ok(false);
        }
        let first = first_sequence.unwrap_or(*sequence);
        if first > expected {
            return Err(HftError::Network(format!(
                "Binance depth sequence gap for {}: expected {}, received {}-{}",
                symbol.as_str(),
                expected,
                first,
                sequence
            )));
        }
        if self.usdm {
            let previous_update_id = previous_update_id.ok_or_else(|| {
                HftError::Network(format!(
                    "Binance USD-M depth update omitted pu for {}",
                    symbol.as_str()
                ))
            })?;
            if self.synchronized_usdm_updates.contains(symbol) && previous_update_id != previous {
                return Err(HftError::Network(format!(
                    "Binance USD-M depth pu gap for {}: expected {}, received {}",
                    symbol.as_str(),
                    previous,
                    previous_update_id
                )));
            }
            self.synchronized_usdm_updates.insert(symbol.clone());
        }
        self.last_update_ids.insert(symbol.clone(), *sequence);
        Ok(true)
    }
}

pub mod capabilities {
    #[derive(Debug, Clone)]
    pub struct BinanceCapabilities {
        pub snapshot_crc: bool,
        pub rest_fallback: bool,
        pub auto_reconnect: bool,
    }

    impl Default for BinanceCapabilities {
        fn default() -> Self {
            Self {
                snapshot_crc: true,
                rest_fallback: true,
                auto_reconnect: true,
            }
        }
    }
}

/// Binance 市場數據流
pub struct BinanceMarketStream {
    caps: capabilities::BinanceCapabilities,
    rest_client: BinanceRestClient,
    is_connected: Arc<AtomicBool>,
    last_heartbeat: Arc<AtomicU64>,
    ws_base_url: String,
    usdm: bool,
}

impl Default for BinanceMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BinanceMarketStream {
    pub fn new() -> Self {
        Self {
            caps: Default::default(),
            rest_client: BinanceRestClient::new(),
            is_connected: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(0)),
            ws_base_url: websocket::WS_BASE_URL.to_string(),
            usdm: false,
        }
    }

    pub fn with_capabilities(caps: capabilities::BinanceCapabilities) -> Self {
        Self {
            caps,
            rest_client: BinanceRestClient::new(),
            is_connected: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(0)),
            ws_base_url: websocket::WS_BASE_URL.to_string(),
            usdm: false,
        }
    }

    pub fn with_ws_base_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.ws_base_url = url;
        self
    }

    pub fn with_rest_base_url(mut self, url: impl Into<String>) -> Self {
        self.rest_client = BinanceRestClient::with_base_url(url);
        self
    }

    pub fn with_usdm(mut self) -> Self {
        self.rest_client = self.rest_client.with_usdm();
        self.usdm = true;
        self
    }

    fn validate_instrument_product(&self, instrument: &InstrumentSpec) -> HftResult<()> {
        match (self.usdm, instrument.product_type) {
            (false, ProductType::Spot | ProductType::TokenizedSecuritySpot)
            | (true, ProductType::Perp) => Ok(()),
            (true, _) => Err(HftError::Network(format!(
                "Binance USD-M market data requires a perpetual instrument: {}",
                instrument.symbol
            ))),
            (false, ProductType::Futures | ProductType::Perp) => Err(HftError::Network(format!(
                "Binance derivatives market data must use the USD-M adapter: {}",
                instrument.symbol
            ))),
            (false, ProductType::BrokerageEquity) => Err(HftError::Network(format!(
                "Binance brokerage equities market data must use an equities adapter: {}",
                instrument.symbol
            ))),
            (false, ProductType::PredictionMarket) => Err(HftError::Network(format!(
                "Binance prediction markets must use the W3W Prediction REST API: {}",
                instrument.symbol
            ))),
        }
    }

    fn event_queue_capacity() -> usize {
        std::env::var("BINANCE_EVENT_QUEUE_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|capacity| *capacity > 0)
            .unwrap_or(DEFAULT_EVENT_QUEUE_CAPACITY)
    }

    fn sync_buffer_capacity() -> usize {
        std::env::var("BINANCE_SYNC_BUFFER_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|capacity| *capacity > 0)
            .unwrap_or(DEFAULT_SYNC_BUFFER_CAPACITY)
    }

    fn snapshot_depth(usdm: bool) -> u16 {
        let configured = std::env::var("BINANCE_SNAPSHOT_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|depth| matches!(*depth, 100 | 500 | 1000 | 5000))
            .unwrap_or(if usdm { 1000 } else { 5000 });
        Self::cap_snapshot_depth(configured, usdm)
    }

    fn cap_snapshot_depth(configured: u16, usdm: bool) -> u16 {
        if usdm {
            configured.min(1000)
        } else {
            configured
        }
    }

    fn uses_ws_snapshot_depth() -> bool {
        websocket::uses_partial_depth_stream()
    }

    fn rest_snapshot_cooldown_remaining(
        last_snapshot_at: Option<tokio::time::Instant>,
    ) -> std::time::Duration {
        last_snapshot_at.map_or(std::time::Duration::ZERO, |previous| {
            REST_SNAPSHOT_COOLDOWN.saturating_sub(previous.elapsed())
        })
    }

    async fn wait_for_rest_snapshot_budget(
        ws_client: &mut BinanceWebSocket,
        last_snapshot_at: Option<tokio::time::Instant>,
    ) -> HftResult<()> {
        let remaining = Self::rest_snapshot_cooldown_remaining(last_snapshot_at);
        if remaining.is_zero() {
            return Ok(());
        }

        let wait = tokio::time::sleep(remaining);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = &mut wait => return Ok(()),
                message = ws_client.receive_message_bytes_with_metrics() => {
                    let (bytes, metrics) = message?.ok_or_else(|| {
                        HftError::Network(
                            "Binance WebSocket closed while REST snapshot budget cooled down"
                                .to_string(),
                        )
                    })?;
                    // Keep the socket drained, but do not publish an unsynchronized generation.
                    let _ = Self::parse_tracked_socket_event(bytes, metrics)?;
                }
            }
        }
    }

    #[cfg(test)]
    fn buffer_during_snapshot_sync(event: MarketEvent) -> Option<MarketEvent> {
        matches!(event, MarketEvent::Update(_)).then_some(event)
    }

    /// 獲取訂單簿快照（用於初始化）
    async fn get_initial_snapshots(
        rest_client: &BinanceRestClient,
        symbols: &[Symbol],
        snapshot_depth: u16,
    ) -> HftResult<Vec<(MarketSnapshot, u64)>> {
        let mut snapshots = Vec::new();

        for symbol in symbols {
            info!("獲取 {:?} 的初始快照", symbol);
            let depth = rest_client.get_depth(symbol, Some(snapshot_depth)).await?;
            let timestamp = now_micros();

            let snapshot =
                MessageConverter::convert_depth_snapshot(symbol.clone(), depth, timestamp)?;

            snapshots.push((snapshot, timestamp));
        }

        Ok(snapshots)
    }

    fn parse_tracked_socket_event(
        bytes: bytes::Bytes,
        mut metrics: WsMessageMetrics,
    ) -> HftResult<Option<ParsedTrackedMarketEvent>> {
        let mut bytes = match bytes.try_into_mut() {
            Ok(bytes) => bytes,
            Err(bytes) => BytesMut::from(bytes.as_ref()),
        };
        let parsed = MessageConverter::parse_stream_message_bytes_with_metadata(&mut bytes)?;
        metrics.mark_parsed();
        Ok(parsed.map(|parsed| {
            let mut event = parsed.event;
            let mut tracker =
                LatencyTracker::from_userspace_websocket_message_delivery(metrics.received_at_us);
            tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
            tracker.record_stage_with_offset(
                LatencyStage::Parsing,
                metrics.parsed_at_us.saturating_sub(metrics.received_at_us),
            );
            let has_exchange_event = event
                .timestamps()
                .and_then(|timestamps| timestamps.exchange_event)
                .is_some();
            let local_receive = metrics.received_at_unix_us.map(LocalReceiveTimestamp::new);
            if let Some(timestamps) = event.timestamps_mut() {
                timestamps.local_receive = local_receive;
            }

            if !has_exchange_event {
                if let Some(local_receive) = local_receive {
                    match &mut event {
                        MarketEvent::Quote(quote) => {
                            quote.timestamp = local_receive.as_micros();
                        }
                        MarketEvent::Snapshot(snapshot) => {
                            snapshot.timestamp = local_receive.as_micros();
                        }
                        _ => {}
                    }
                }
            }

            ParsedTrackedMarketEvent {
                event,
                tracker,
                previous_update_id: parsed.previous_update_id,
            }
        }))
    }

    async fn synchronize_books(
        ws_client: &mut BinanceWebSocket,
        rest_client: &BinanceRestClient,
        symbols: &[Symbol],
        buffer_capacity: usize,
        snapshot_depth: u16,
    ) -> HftResult<(
        Vec<(MarketSnapshot, u64)>,
        VecDeque<ParsedTrackedMarketEvent>,
    )> {
        let snapshot_future = Self::get_initial_snapshots(rest_client, symbols, snapshot_depth);
        tokio::pin!(snapshot_future);
        let mut buffered_events = VecDeque::with_capacity(buffer_capacity.min(1024));

        loop {
            tokio::select! {
                snapshots = &mut snapshot_future => {
                    return snapshots.map(|snapshots| (snapshots, buffered_events));
                }
                message = ws_client.receive_message_bytes_with_metrics() => {
                    let (bytes, metrics) = message?.ok_or_else(|| {
                        HftError::Network("Binance WebSocket closed during snapshot sync".to_string())
                    })?;
                    if let Some(event) = Self::parse_tracked_socket_event(bytes, metrics)? {
                        if !matches!(&event.event, MarketEvent::Update(_)) {
                            continue;
                        }
                        if buffered_events.len() >= buffer_capacity {
                            return Err(HftError::Network(format!(
                                "Binance snapshot sync buffer exceeded {} events",
                                buffer_capacity
                            )));
                        }
                        buffered_events.push_back(event);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl MarketStream for BinanceMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let stream = self.subscribe_tracked(symbols).await?;
        Ok(Box::pin(
            stream.map(|result| result.map(|tracked| tracked.event)),
        ))
    }

    async fn subscribe_tracked(
        &self,
        symbols: Vec<Symbol>,
    ) -> HftResult<BoxStream<TrackedMarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("品種列表不能為空"));
        }

        info!("訂閱 Binance 市場數據，品種: {:?}", symbols);

        let uses_ws_snapshot_depth = Self::uses_ws_snapshot_depth();
        if !uses_ws_snapshot_depth && (!self.caps.snapshot_crc || !self.caps.rest_fallback) {
            return Err(HftError::Config(
                "Binance diff-depth requires the REST snapshot bridge; use partial20 for a WebSocket-only feed"
                    .to_string(),
            ));
        }

        let event_queue_capacity = Self::event_queue_capacity();
        let (tx, rx) = mpsc::channel(event_queue_capacity);
        let generation = Arc::new(AtomicU64::new(1));
        let (control_tx, control_rx) = watch::channel(None);

        // Default mode is a WebSocket-only partial-depth snapshot stream. Full diff-depth mode is
        // opt-in and uses one rate-budgeted REST snapshot while explicitly buffering WS events.
        let mut ws_client = BinanceWebSocket::with_base_url(self.ws_base_url.clone());
        let rest_client = self.rest_client.clone();
        let snapshot_enabled = !uses_ws_snapshot_depth;
        let snapshot_depth = Self::snapshot_depth(self.usdm);
        let usdm = self.usdm;
        let sync_buffer_capacity = Self::sync_buffer_capacity();
        let auto_reconnect = self.caps.auto_reconnect;
        let is_connected = Arc::clone(&self.is_connected);
        let last_heartbeat = Arc::clone(&self.last_heartbeat);
        let task_generation = Arc::clone(&generation);

        tokio::spawn(async move {
            use adapters_common::calculate_exponential_backoff;
            use adapters_common::ws_helpers::constants::DEFAULT_BASE_DELAY_MS;

            let mut attempts: u32 = 0;
            let mut last_rest_snapshot_at: Option<tokio::time::Instant> = None;

            loop {
                let mut invalidation_error = None;
                let mut invalidation_reason =
                    "Binance public stream requires a fresh synchronized snapshot".to_string();
                match ws_client.connect_and_subscribe(symbols.clone()).await {
                    Ok(()) => {
                        let (snapshots, mut buffered_events) = if snapshot_enabled {
                            let synchronized = async {
                                BinanceMarketStream::wait_for_rest_snapshot_budget(
                                    &mut ws_client,
                                    last_rest_snapshot_at,
                                )
                                .await?;
                                last_rest_snapshot_at = Some(tokio::time::Instant::now());
                                BinanceMarketStream::synchronize_books(
                                    &mut ws_client,
                                    &rest_client,
                                    &symbols,
                                    sync_buffer_capacity,
                                    snapshot_depth,
                                )
                                .await
                            }
                            .await;
                            match synchronized {
                                Ok(synchronized) => synchronized,
                                Err(error) => {
                                    invalidation_reason =
                                        "Binance REST snapshot bridge failed".to_string();
                                    invalidation_error = Some(error);
                                    (Vec::new(), VecDeque::new())
                                }
                            }
                        } else {
                            (Vec::new(), VecDeque::new())
                        };

                        if snapshot_enabled && snapshots.is_empty() {
                            // Synchronization failed; invalidate the previous venue book below.
                        } else {
                            let mut sequence_tracker = snapshot_enabled.then(|| {
                                DepthSequenceTracker::from_snapshots(
                                    snapshots.iter().map(|(snapshot, _)| snapshot),
                                    usdm,
                                )
                            });
                            'generation: {
                                for (snapshot, completed_at_us) in snapshots {
                                    let mut tracker = LatencyTracker::from_time(completed_at_us);
                                    tracker.capture_boundary =
                                        hft_core::LatencyCaptureBoundary::SnapshotCompletion;
                                    match try_queue_market_event(
                                        &tx,
                                        &task_generation,
                                        TrackedMarketEvent {
                                            event: MarketEvent::Snapshot(snapshot),
                                            tracker,
                                        },
                                    ) {
                                        Ok(()) => {}
                                        Err(QueuePublishError::Closed) => return,
                                        Err(QueuePublishError::Full) => {
                                            invalidation_reason = format!(
                                                "Binance event queue exceeded {event_queue_capacity} events; rebuilding from snapshot"
                                            );
                                            invalidation_error = Some(HftError::Network(
                                                invalidation_reason.clone(),
                                            ));
                                            break 'generation;
                                        }
                                    }
                                }

                                while let Some(event) = buffered_events.pop_front() {
                                    let forward = match sequence_tracker.as_mut() {
                                        Some(tracker) => tracker.validate_and_advance(
                                            &event.event,
                                            event.previous_update_id,
                                        ),
                                        None => Ok(true),
                                    };
                                    match forward {
                                        Ok(true) => {
                                            match try_queue_market_event(
                                                &tx,
                                                &task_generation,
                                                event.into_tracked(),
                                            ) {
                                                Ok(()) => {}
                                                Err(QueuePublishError::Closed) => return,
                                                Err(QueuePublishError::Full) => {
                                                    invalidation_reason = format!(
                                                        "Binance event queue exceeded {event_queue_capacity} events; rebuilding from snapshot"
                                                    );
                                                    invalidation_error = Some(HftError::Network(
                                                        invalidation_reason.clone(),
                                                    ));
                                                    break 'generation;
                                                }
                                            }
                                        }
                                        Ok(false) => {}
                                        Err(error) => {
                                            invalidation_reason =
                                                "Binance buffered depth sequence gap".to_string();
                                            invalidation_error = Some(error);
                                            break 'generation;
                                        }
                                    }
                                }

                                is_connected.store(true, Ordering::Release);
                                last_heartbeat.store(now_micros(), Ordering::Release);
                                let live_since = tokio::time::Instant::now();
                                loop {
                                    match ws_client.receive_message_bytes_with_metrics().await {
                                        Ok(Some((bytes, metrics))) => {
                                            if live_since.elapsed()
                                                >= std::time::Duration::from_secs(30)
                                            {
                                                attempts = 0;
                                            }
                                            last_heartbeat.store(now_micros(), Ordering::Release);
                                            match BinanceMarketStream::parse_tracked_socket_event(
                                                bytes, metrics,
                                            ) {
                                                Ok(Some(event)) => {
                                                    let forward = match sequence_tracker.as_mut() {
                                                        Some(tracker) => tracker
                                                            .validate_and_advance(
                                                                &event.event,
                                                                event.previous_update_id,
                                                            ),
                                                        None => Ok(true),
                                                    };
                                                    match forward {
                                                        Ok(true) => {
                                                            match try_queue_market_event(
                                                                &tx,
                                                                &task_generation,
                                                                event.into_tracked(),
                                                            ) {
                                                                Ok(()) => {}
                                                                Err(QueuePublishError::Closed) => {
                                                                    return
                                                                }
                                                                Err(QueuePublishError::Full) => {
                                                                    invalidation_reason = format!(
                                                                        "Binance event queue exceeded {event_queue_capacity} events; rebuilding from snapshot"
                                                                    );
                                                                    invalidation_error =
                                                                        Some(HftError::Network(
                                                                            invalidation_reason
                                                                                .clone(),
                                                                        ));
                                                                    break 'generation;
                                                                }
                                                            }
                                                        }
                                                        Ok(false) => {}
                                                        Err(error) => {
                                                            invalidation_reason =
                                                                "Binance live depth sequence gap"
                                                                    .to_string();
                                                            invalidation_error = Some(error);
                                                            break 'generation;
                                                        }
                                                    }
                                                }
                                                Ok(None) => {}
                                                Err(error) => {
                                                    invalidation_reason =
                                                        "Binance market frame rejected".to_string();
                                                    invalidation_error = Some(error);
                                                    break 'generation;
                                                }
                                            }
                                        }
                                        Ok(None) => {
                                            warn!("Binance WS 關閉，準備重連");
                                            break;
                                        }
                                        Err(error) => {
                                            error!("Binance WS 錯誤: {}，準備重連", error);
                                            invalidation_error = Some(error);
                                            break 'generation;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        error!("Binance WS 連接失敗: {}", error);
                        invalidation_reason =
                            "Binance public WebSocket connection failed".to_string();
                        invalidation_error = Some(error);
                    }
                }

                is_connected.store(false, Ordering::Release);
                publish_invalidation(
                    &task_generation,
                    &control_tx,
                    invalidation_reason,
                    invalidation_error,
                );
                if control_tx.is_closed() {
                    return;
                }

                if !auto_reconnect {
                    return;
                }
                attempts += 1;
                let delay = calculate_exponential_backoff(attempts, DEFAULT_BASE_DELAY_MS);
                tokio::time::sleep(delay).await;
            }
        });

        Ok(multiplex_market_events(rx, control_rx, generation))
    }

    async fn subscribe_instruments(
        &self,
        instruments: Vec<InstrumentSpec>,
    ) -> HftResult<BoxStream<MarketEvent>> {
        if instruments.is_empty() {
            return Err(HftError::new("商品列表不能為空"));
        }

        for instrument in &instruments {
            self.validate_instrument_product(instrument)?;
        }

        info!("訂閱 Binance 商品市場數據: {:?}", instruments);
        self.subscribe(
            instruments
                .into_iter()
                .map(|instrument| instrument.symbol)
                .collect(),
        )
        .await
    }

    async fn subscribe_tracked_instruments(
        &self,
        instruments: Vec<InstrumentSpec>,
    ) -> HftResult<BoxStream<TrackedMarketEvent>> {
        if instruments.is_empty() {
            return Err(HftError::new("商品列表不能為空"));
        }
        for instrument in &instruments {
            self.validate_instrument_product(instrument)?;
        }
        self.subscribe_tracked(
            instruments
                .into_iter()
                .map(|instrument| instrument.symbol)
                .collect(),
        )
        .await
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.is_connected.load(Ordering::Acquire),
            latency_ms: None,
            last_heartbeat: self.last_heartbeat.load(Ordering::Acquire),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        // `subscribe` owns the WebSocket lifecycle. Do not spend REST request weight merely to
        // probe connectivity; health becomes true only after synchronized market data is live.
        self.is_connected.store(false, Ordering::Release);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.is_connected.store(false, Ordering::Release);
        info!("Binance 適配器已斷開");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capabilities::BinanceCapabilities;
    use futures::StreamExt;

    #[test]
    fn test_binance_market_stream_default() {
        let stream = BinanceMarketStream::default();
        assert!(!stream.is_connected.load(Ordering::Acquire));
        assert_eq!(stream.last_heartbeat.load(Ordering::Acquire), 0);
        assert!(stream.caps.snapshot_crc);
        assert!(stream.caps.rest_fallback);
        assert!(stream.caps.auto_reconnect);
    }

    #[test]
    fn test_binance_market_stream_new() {
        let stream = BinanceMarketStream::new();
        assert!(!stream.is_connected.load(Ordering::Acquire));
        assert_eq!(stream.ws_base_url, websocket::WS_BASE_URL);
    }

    #[test]
    fn test_binance_capabilities_default() {
        let caps = BinanceCapabilities::default();
        assert!(caps.snapshot_crc);
        assert!(caps.rest_fallback);
        assert!(caps.auto_reconnect);
    }

    #[test]
    fn test_binance_capabilities_custom() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: false,
            auto_reconnect: true,
        };
        assert!(!caps.snapshot_crc);
        assert!(!caps.rest_fallback);
        assert!(caps.auto_reconnect);
    }

    #[test]
    fn test_with_capabilities() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: true,
            auto_reconnect: false,
        };
        let stream = BinanceMarketStream::with_capabilities(caps.clone());
        assert_eq!(stream.caps.snapshot_crc, caps.snapshot_crc);
        assert_eq!(stream.caps.rest_fallback, caps.rest_fallback);
        assert_eq!(stream.caps.auto_reconnect, caps.auto_reconnect);
    }

    #[test]
    fn test_with_ws_base_url() {
        let stream = BinanceMarketStream::new().with_ws_base_url("wss://custom.binance.com/ws");
        assert_eq!(stream.ws_base_url, "wss://custom.binance.com/ws");
    }

    #[test]
    fn test_with_rest_base_url() {
        let stream = BinanceMarketStream::new().with_rest_base_url("https://custom.binance.com");
        // The rest_client is updated internally
        assert!(!stream.is_connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_health_check_initial_state() {
        let stream = BinanceMarketStream::new();
        let health = stream.health().await;

        assert!(!health.connected);
        assert!(health.latency_ms.is_none());
        assert_eq!(health.last_heartbeat, 0);
    }

    #[tokio::test]
    async fn test_disconnect() {
        let mut stream = BinanceMarketStream::new();
        stream.is_connected.store(true, Ordering::Release);

        let result = stream.disconnect().await;
        assert!(result.is_ok());
        assert!(!stream.is_connected.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_fails() {
        let stream = BinanceMarketStream::new();
        let result = stream.subscribe(vec![]).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires live Binance public WebSocket"]
    async fn live_ws_only_mode_delivers_depth_and_realtime_quote() {
        let ws_url = std::env::var("BINANCE_WS_SMOKE_URL")
            .unwrap_or_else(|_| websocket::WS_BASE_URL.to_string());
        let stream = BinanceMarketStream::new().with_ws_base_url(ws_url);
        let mut events = stream
            .subscribe(vec![Symbol::new("BTCUSDT")])
            .await
            .expect("public subscription");

        let (saw_depth, saw_quote, saw_trade) =
            tokio::time::timeout(std::time::Duration::from_secs(15), async move {
                let mut saw_depth = false;
                let mut saw_quote = false;
                let mut saw_trade = false;
                while let Some(event) = events.next().await {
                    match event.expect("valid public event") {
                        MarketEvent::Snapshot(_) => saw_depth = true,
                        MarketEvent::Quote(_) => saw_quote = true,
                        MarketEvent::Trade(_) => saw_trade = true,
                        _ => {}
                    }
                    if saw_depth && saw_quote && saw_trade {
                        break;
                    }
                }
                (saw_depth, saw_quote, saw_trade)
            })
            .await
            .expect("Binance public feed timeout");

        assert!(saw_depth, "expected WebSocket L20 snapshot");
        assert!(saw_quote, "expected real-time bookTicker quote");
        assert!(saw_trade, "expected real-time raw trade");
    }

    #[tokio::test]
    async fn subscribe_instruments_rejects_brokerage_equity_on_spot_adapter() {
        let stream = BinanceMarketStream::new();
        let instrument = InstrumentSpec {
            symbol: Symbol::new("AAPL"),
            venue: hft_core::VenueId::BINANCE_BROKERAGE_EQUITIES,
            asset_class: hft_core::AssetClass::Equity,
            product_type: ProductType::BrokerageEquity,
            regulatory_profile: hft_core::RegulatoryProfile::RestrictedJurisdiction,
            underlying_symbol: Some("AAPL".to_string()),
            issuer: Some("AAPL".to_string()),
            quote_currency: Some("USD".to_string()),
        };

        let result = stream.subscribe_instruments(vec![instrument]).await;
        assert!(matches!(result, Err(err) if err.to_string().contains("equities adapter")));
    }

    #[test]
    fn instrument_product_must_match_spot_or_usdm_mode() {
        let spot = InstrumentSpec::crypto_spot(Symbol::new("BTCUSDT"), hft_core::VenueId::BINANCE);
        let mut perp = spot.clone();
        perp.product_type = ProductType::Perp;

        let spot_stream = BinanceMarketStream::new();
        assert!(spot_stream.validate_instrument_product(&spot).is_ok());
        assert!(spot_stream.validate_instrument_product(&perp).is_err());

        let usdm_stream = BinanceMarketStream::new().with_usdm();
        assert!(usdm_stream.validate_instrument_product(&perp).is_ok());
        assert!(usdm_stream.validate_instrument_product(&spot).is_err());
    }

    #[test]
    fn test_capabilities_debug() {
        let caps = BinanceCapabilities::default();
        let debug_str = format!("{:?}", caps);
        assert!(debug_str.contains("BinanceCapabilities"));
        assert!(debug_str.contains("snapshot_crc"));
    }

    #[test]
    fn test_capabilities_clone() {
        let caps = BinanceCapabilities {
            snapshot_crc: false,
            rest_fallback: true,
            auto_reconnect: true,
        };
        let cloned = caps.clone();
        assert_eq!(cloned.snapshot_crc, caps.snapshot_crc);
        assert_eq!(cloned.rest_fallback, caps.rest_fallback);
        assert_eq!(cloned.auto_reconnect, caps.auto_reconnect);
    }

    #[test]
    fn diff_depth_tracker_bridges_snapshot_and_rejects_gap() {
        let symbol = Symbol::new("BTCUSDT");
        let snapshots = vec![MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: 1,
            bids: Vec::new(),
            asks: Vec::new(),
            sequence: 100,
            source_venue: Some(hft_core::VenueId::BINANCE),
            timestamps: Default::default(),
        }];
        let mut tracker = DepthSequenceTracker::from_snapshots(&snapshots, false);
        let update = |first_sequence, sequence| {
            MarketEvent::Update(BookUpdate {
                symbol: symbol.clone(),
                timestamp: 2,
                bids: Vec::new(),
                asks: Vec::new(),
                first_sequence: Some(first_sequence),
                sequence,
                is_snapshot: false,
                source_venue: Some(hft_core::VenueId::BINANCE),
                timestamps: Default::default(),
            })
        };

        assert!(!tracker
            .validate_and_advance(&update(90, 100), None)
            .unwrap());
        assert!(tracker
            .validate_and_advance(&update(99, 101), None)
            .unwrap());
        assert!(tracker
            .validate_and_advance(&update(103, 103), None)
            .is_err());
    }

    #[test]
    fn usdm_diff_depth_requires_each_pu_to_match_the_previous_u() {
        let symbol = Symbol::new("BTCUSDT");
        let snapshots = vec![MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: 1,
            bids: Vec::new(),
            asks: Vec::new(),
            sequence: 100,
            source_venue: Some(hft_core::VenueId::BINANCE),
            timestamps: Default::default(),
        }];
        let update = |first_sequence, sequence| {
            MarketEvent::Update(BookUpdate {
                symbol: symbol.clone(),
                timestamp: 2,
                bids: Vec::new(),
                asks: Vec::new(),
                first_sequence: Some(first_sequence),
                sequence,
                is_snapshot: false,
                source_venue: Some(hft_core::VenueId::BINANCE),
                timestamps: Default::default(),
            })
        };
        let mut tracker = DepthSequenceTracker::from_snapshots(&snapshots, true);

        assert!(tracker
            .validate_and_advance(&update(99, 101), Some(98))
            .unwrap());
        assert!(tracker
            .validate_and_advance(&update(102, 103), Some(101))
            .unwrap());
        assert!(tracker
            .validate_and_advance(&update(103, 104), Some(100))
            .is_err());
    }

    #[test]
    fn usdm_rest_snapshot_depth_never_exceeds_one_thousand() {
        assert_eq!(BinanceMarketStream::cap_snapshot_depth(5000, true), 1000);
        assert_eq!(BinanceMarketStream::cap_snapshot_depth(5000, false), 5000);
    }

    #[test]
    fn snapshot_recovery_buffers_depth_only() {
        let update = MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 2,
            bids: Vec::new(),
            asks: Vec::new(),
            first_sequence: Some(101),
            sequence: 101,
            is_snapshot: false,
            source_venue: Some(hft_core::VenueId::BINANCE),
            timestamps: Default::default(),
        });
        let quote = MarketEvent::Quote(ports::TopOfBook {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 2,
            sequence: 101,
            bid: ports::BookLevel::new_unchecked(100.0, 1.0),
            ask: ports::BookLevel::new_unchecked(101.0, 1.0),
            source_venue: Some(hft_core::VenueId::BINANCE),
            timestamps: Default::default(),
        });

        assert!(BinanceMarketStream::buffer_during_snapshot_sync(update).is_some());
        assert!(BinanceMarketStream::buffer_during_snapshot_sync(quote).is_none());
    }

    #[test]
    fn rest_snapshot_cooldown_is_scoped_to_snapshot_fetch() {
        let previous = tokio::time::Instant::now();
        let remaining = BinanceMarketStream::rest_snapshot_cooldown_remaining(Some(previous));

        assert!(remaining > std::time::Duration::ZERO);
        assert!(remaining <= std::time::Duration::from_secs(60));
    }

    #[tokio::test]
    async fn invalidation_discards_queued_old_generation_before_fresh_data() {
        let generation = Arc::new(AtomicU64::new(1));
        let (data_tx, data_rx) = mpsc::channel(2);
        let (control_tx, control_rx) = watch::channel(None);
        let snapshot = |sequence| {
            TrackedMarketEvent::new(MarketEvent::Snapshot(MarketSnapshot {
                symbol: Symbol::new("BTCUSDT"),
                timestamp: sequence,
                bids: Vec::new(),
                asks: Vec::new(),
                sequence,
                source_venue: Some(hft_core::VenueId::BINANCE),
                timestamps: Default::default(),
            }))
        };
        data_tx
            .try_send(QueuedMarketEvent {
                generation: 1,
                event: snapshot(1),
            })
            .unwrap();
        publish_invalidation(&generation, &control_tx, "queue overflow".to_string(), None);
        data_tx
            .try_send(QueuedMarketEvent {
                generation: 2,
                event: snapshot(2),
            })
            .unwrap();
        drop(data_tx);
        drop(control_tx);

        let mut stream = multiplex_market_events(data_rx, control_rx, generation);
        assert!(matches!(
            stream.next().await,
            Some(Ok(TrackedMarketEvent {
                event: MarketEvent::Disconnect { .. },
                ..
            }))
        ));
        let Some(Ok(TrackedMarketEvent {
            event: MarketEvent::Snapshot(snapshot),
            ..
        })) = stream.next().await
        else {
            panic!("expected fresh snapshot after invalidation");
        };
        assert_eq!(snapshot.sequence, 2);
    }

    #[test]
    fn tracked_parser_preserves_receive_to_parse_latency() {
        let received_at = hft_core::monotonic_micros();
        let message = bytes::Bytes::from_static(
            br#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":101,"bids":[["45000.00","0.1"]],"asks":[["45100.00","0.2"]]}}"#,
        );

        let tracked = BinanceMarketStream::parse_tracked_socket_event(
            message,
            WsMessageMetrics::new(received_at, 0),
        )
        .unwrap()
        .expect("tracked depth event");

        assert!(matches!(tracked.event, MarketEvent::Snapshot(_)));
        assert!(tracked
            .tracker
            .get_measurement(LatencyStage::Parsing)
            .is_some());
    }

    #[test]
    fn tracked_timestamps_keep_depth_e_trade_e_t_and_local_receive_distinct() {
        let received_at_unix_us = 123_456_999_000;
        let parse = |message| {
            BinanceMarketStream::parse_tracked_socket_event(
                bytes::Bytes::from_static(message),
                WsMessageMetrics::new_with_unix(
                    hft_core::monotonic_micros(),
                    received_at_unix_us,
                    0,
                ),
            )
            .unwrap()
            .expect("tracked Binance event")
        };

        let depth = parse(
            br#"{"stream":"btcusdt@depth","data":{"e":"depthUpdate","E":123456789,"s":"BTCUSDT","U":100,"u":101,"pu":99,"b":[["45000.00","0.1"]],"a":[["45100.00","0.2"]]}}"#,
        );
        assert_eq!(depth.previous_update_id, Some(99));
        assert_eq!(
            depth
                .event
                .timestamps()
                .unwrap()
                .exchange_event
                .map(|timestamp| timestamp.as_micros()),
            Some(123456789000)
        );
        assert!(depth.event.timestamps().unwrap().exchange_trade.is_none());
        assert!(depth.event.timestamps().unwrap().local_receive.is_some());

        let partial_depth = parse(
            br#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":101,"bids":[["45000.00","0.1"]],"asks":[["45100.00","0.2"]]}}"#,
        );
        let MarketEvent::Snapshot(snapshot) = partial_depth.event else {
            panic!("expected partial depth snapshot");
        };
        assert_eq!(snapshot.timestamp, received_at_unix_us);

        let trade = parse(
            br#"{"stream":"btcusdt@trade","data":{"e":"trade","E":123456790,"s":"BTCUSDT","t":12345,"p":"45000.00","q":"0.1","T":123456789,"m":false}}"#,
        );
        assert_eq!(
            trade
                .event
                .timestamps()
                .unwrap()
                .exchange_event
                .map(|timestamp| timestamp.as_micros()),
            Some(123456790000)
        );
        assert_eq!(
            trade
                .event
                .timestamps()
                .unwrap()
                .exchange_trade
                .map(|timestamp| timestamp.as_micros()),
            Some(123456789000)
        );

        let ticker = parse(
            br#"{"stream":"btcusdt@bookTicker","data":{"u":400900217,"s":"BTCUSDT","b":"25.35190000","B":"31.21000000","a":"25.36520000","A":"40.66000000"}}"#,
        );
        assert!(ticker.event.timestamps().unwrap().exchange_event.is_none());
        assert!(ticker.event.timestamps().unwrap().exchange_trade.is_none());
        let local_receive = ticker
            .event
            .timestamps()
            .unwrap()
            .local_receive
            .expect("local receive timestamp");
        let MarketEvent::Quote(quote) = ticker.event else {
            panic!("expected bookTicker quote");
        };
        assert_eq!(quote.timestamp, local_receive.as_micros());

        let kline = parse(
            br#"{"stream":"btcusdt@kline_1m","data":{"e":"kline","E":123456791,"s":"BTCUSDT","k":{"t":123456000,"T":123515999,"s":"BTCUSDT","i":"1m","f":100,"L":200,"o":"45000.00","c":"45001.00","h":"45002.00","l":"44999.00","v":"1.0","n":10,"x":false,"q":"45000.0","V":"0.5","Q":"22500.0","B":"0"}}}"#,
        );
        assert_eq!(
            kline
                .event
                .timestamps()
                .unwrap()
                .exchange_event
                .map(|timestamp| timestamp.as_micros()),
            Some(123456791000)
        );
        assert_eq!(
            kline
                .event
                .timestamps()
                .unwrap()
                .local_receive
                .map(|timestamp| timestamp.as_micros()),
            Some(received_at_unix_us)
        );
    }
}
