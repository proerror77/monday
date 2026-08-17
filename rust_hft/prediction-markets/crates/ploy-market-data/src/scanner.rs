//! Market scanner that discovers active Polymarket markets.
//!
//! Crypto discovery still emits the existing `EventDiscovered` /
//! `EventExpired` runtime updates so the current strategy logic remains intact.
//! In parallel, the scanner now persists normalized market descriptors into
//! `pm_market_catalog`, including low-frequency sports discovery for later
//! capture and replay work.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use ploy_market_contracts::MarketUpdate;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::discovery::crypto::DiscoveredCryptoMarket;
use crate::discovery::crypto::{discover_crypto_catalog_markets, discover_crypto_markets};
use crate::discovery::sports::discover_sports_markets;
use crate::discovery::upsert_market_catalog;
use crate::feeds::spawn_clob_ws_quote_feed_until;
use crate::gamma_keyset::fetch_markets;
use crate::reference_prices::{new_reference_price_registry, ReferencePriceRegistry};

const SCAN_INTERVAL_SECS: u64 = 30;
const SPORTS_DISCOVERY_REFRESH_SECS: i64 = 300;
const SPORTS_DISCOVERY_LIMIT: i32 = 500;
const DEFAULT_DISCOVERY_LOOKAHEAD_MINUTES: i64 = 20;
const CRYPTO_DISCOVERY_PAGE_LIMIT: i32 = 100;
const CRYPTO_DISCOVERY_MAX_MARKETS: usize = 1_200;
const QUOTE_FEED_GRACE_SECS: i64 = 90;
/// How far back to look for open positions that need recovery on startup.
const RECOVERY_LOOKBACK_HOURS: i64 = 48;

struct TrackedEvent {
    end_time: chrono::DateTime<Utc>,
    symbol: String,
    price_to_beat: Option<Decimal>,
}

/// Configuration for the central Polymarket market-discovery collector.
///
/// This collector owns Gamma/catalog discovery for live infrastructure. Strategy
/// runners should consume the persisted rows instead of opening their own Gamma
/// scanners.
#[derive(Debug, Clone)]
pub struct MarketDiscoveryCollectorConfig {
    pub symbols: Vec<String>,
    pub refresh_interval_secs: u64,
    pub lookahead_minutes: i64,
    pub capture_sports_catalog: bool,
}

impl MarketDiscoveryCollectorConfig {
    #[must_use]
    pub fn with_safe_defaults(mut self) -> Self {
        if self.symbols.is_empty() {
            self.symbols = vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
            ];
        }
        if self.refresh_interval_secs == 0 {
            self.refresh_interval_secs = SCAN_INTERVAL_SECS;
        }
        if self.lookahead_minutes <= 0 {
            self.lookahead_minutes = DEFAULT_DISCOVERY_LOOKAHEAD_MINUTES;
        }
        self
    }
}

/// Run the central market-discovery loop.
///
/// This intentionally persists catalog/metadata only. It does not emit runtime
/// events or spawn quote feeds; dedicated collector services own those concerns.
pub async fn run_market_discovery_collector(config: MarketDiscoveryCollectorConfig, pool: PgPool) {
    let config = config.with_safe_defaults();
    let client = GammaClient::default();
    let reference_prices = new_reference_price_registry();
    let mut last_sports_refresh: Option<DateTime<Utc>> = None;

    info!(
        symbols = ?config.symbols,
        refresh_secs = config.refresh_interval_secs,
        lookahead_minutes = config.lookahead_minutes,
        capture_sports_catalog = config.capture_sports_catalog,
        "Starting Polymarket market-discovery collector"
    );

    loop {
        let now = Utc::now();
        let discovered = refresh_crypto_catalog(
            &client,
            &pool,
            &reference_prices,
            &config.symbols,
            now,
            config.lookahead_minutes,
        )
        .await;

        info!(discovered, "Polymarket market-discovery refresh complete");

        if should_refresh_sports_catalog(
            now,
            last_sports_refresh,
            true,
            config.capture_sports_catalog,
        ) {
            refresh_sports_catalog(&client, Some(&pool)).await;
            last_sports_refresh = Some(now);
        }

        tokio::time::sleep(std::time::Duration::from_secs(config.refresh_interval_secs)).await;
    }
}

/// Spawn a background task that periodically discovers active 5-min binary
/// option markets, injects lifecycle events, and spawns quote feeds for
/// newly discovered token IDs.
///
/// Uses the reference-price registry to populate price_to_beat with the most
/// recent Chainlink price for each symbol.
///
/// When `pool` is provided, newly discovered markets are upserted into
/// `pm_market_metadata` so that historical backtests can replay the same data.
pub fn spawn_market_scanner(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
    capture_sports_catalog: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = GammaClient::default();
        let mut tracked: HashMap<String, TrackedEvent> = HashMap::new();
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut quote_handles: Vec<JoinHandle<()>> = Vec::new();
        let mut last_sports_refresh: Option<DateTime<Utc>> = None;

        // On startup, recover any open positions from events that expired while
        // the runner was offline. These will never receive a new EventExpired from
        // the live scanner, so we emit them now with official settlement outcomes.
        if let Some(ref db) = pool {
            recover_expired_open_positions(&tx, db).await;
            recover_pending_open_positions(
                tx.clone(),
                db,
                &mut tracked,
                &mut subscribed_tokens,
                &mut quote_handles,
            )
            .await;
        }

        loop {
            let now = Utc::now();

            expire_tracked_events(&tx, &mut tracked, pool.as_ref(), now).await;

            let request = crypto_markets_request(now, DEFAULT_DISCOVERY_LOOKAHEAD_MINUTES);

            match fetch_markets(&request, CRYPTO_DISCOVERY_MAX_MARKETS).await {
                Ok(markets) => {
                    let mut new_tokens: Vec<U256> = Vec::new();
                    let mut quote_stop_at: Option<DateTime<Utc>> = None;
                    let discovered =
                        discover_crypto_markets(&markets, &symbols, &reference_prices, now).await;

                    if discovered.is_empty() {
                        debug!("No active crypto markets in the next 6 minutes");
                    }

                    for market in discovered {
                        persist_discovered_crypto_market(pool.as_ref(), &market).await;

                        if tracked.contains_key(&market.compatibility_event_id) {
                            continue;
                        }

                        let Some(up_asset_id) = parse_token_id(&market.up_token) else {
                            warn!(
                                market_id = %market.descriptor.market_id,
                                token_id = %market.up_token,
                                "Skipping discovered market with invalid up token id"
                            );
                            continue;
                        };
                        let Some(down_asset_id) = parse_token_id(&market.down_token) else {
                            warn!(
                                market_id = %market.descriptor.market_id,
                                token_id = %market.down_token,
                                "Skipping discovered market with invalid down token id"
                            );
                            continue;
                        };

                        if subscribed_tokens.insert(market.up_token.clone()) {
                            new_tokens.push(up_asset_id);
                            quote_stop_at =
                                extend_quote_feed_stop_at(quote_stop_at, market.end_time);
                        }
                        if subscribed_tokens.insert(market.down_token.clone()) {
                            new_tokens.push(down_asset_id);
                            quote_stop_at =
                                extend_quote_feed_stop_at(quote_stop_at, market.end_time);
                        }

                        let end_time = market.end_time.clone();
                        let window_secs = market.market_window_secs;
                        let price_to_beat = market.price_to_beat.clone();

                        tracked.insert(
                            market.compatibility_event_id.clone(),
                            TrackedEvent {
                                end_time,
                                symbol: market.symbol.clone(),
                                price_to_beat: price_to_beat.clone(),
                            },
                        );

                        let _ = tx.send(MarketUpdate::EventDiscovered {
                            event_id: Arc::from(market.compatibility_event_id.as_str()),
                            symbol: Arc::from(market.symbol.as_str()),
                            up_token: Arc::from(market.up_token.as_str()),
                            down_token: Arc::from(market.down_token.as_str()),
                            end_time,
                            window_secs,
                            price_to_beat,
                            resolved_up_won: None,
                        });
                    }

                    if !new_tokens.is_empty() {
                        info!(
                            new_tokens = new_tokens.len(),
                            total_tracked = tracked.len(),
                            "Discovered new markets, subscribing to quotes",
                        );
                        quote_handles.push(spawn_clob_ws_quote_feed_until(
                            tx.clone(),
                            new_tokens,
                            quote_stop_at,
                        ));
                    } else {
                        debug!(tracked = tracked.len(), "Scanner poll: no new markets");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Gamma markets query failed, will retry");
                }
            }

            if should_refresh_sports_catalog(
                now,
                last_sports_refresh,
                pool.is_some(),
                capture_sports_catalog,
            ) {
                refresh_sports_catalog(&client, pool.as_ref()).await;
                last_sports_refresh = Some(now);
            }

            quote_handles.retain(|h| !h.is_finished());

            tokio::time::sleep(std::time::Duration::from_secs(SCAN_INTERVAL_SECS)).await;
        }
    })
}

/// On startup, find events that expired while the runner was offline and still
/// have open positions in `strategy_runtime_fills`. Emit `EventExpired` with
/// the official settlement outcome from `pm_token_settlements` so the strategy
/// can close those positions immediately.
async fn recover_expired_open_positions(tx: &broadcast::Sender<MarketUpdate>, pool: &PgPool) {
    let lookback = Utc::now() - Duration::hours(RECOVERY_LOOKBACK_HOURS);

    // Find open positions: BUY fills with no matching SELL, for events that
    // ended before now. Join pm_market_metadata for end_time and
    // pm_token_settlements for the official outcome.
    let rows: Vec<(String, String, DateTime<Utc>, Option<bool>)> = match sqlx::query_as(
        r#"
        SELECT DISTINCT
            f.event_id,
            COALESCE(f.symbol, '') AS symbol,
            COALESCE(m.end_time, NOW() - INTERVAL '1 second') AS end_time,
            CASE
                WHEN winner.token_id IS NOT NULL THEN
                    winner.token_id = trim(both '"' from ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0))
                ELSE NULL
            END AS resolved_up_won
        FROM strategy_runtime_fills f
        LEFT JOIN pm_market_metadata m ON m.market_slug = f.event_id
        LEFT JOIN pm_token_settlements winner
            ON winner.market_slug = f.event_id
           AND winner.resolved
           AND winner.settled_price >= 0.99
        WHERE f.fill_side = 'BUY'
          AND f.fill_timestamp >= $1
          AND COALESCE(m.end_time, NOW()) < NOW()
          AND NOT EXISTS (
              SELECT 1 FROM strategy_runtime_fills s2
              WHERE s2.token_id = f.token_id
                AND s2.fill_side = 'SELL'
                AND s2.intent_id LIKE 'settle_%'
          )
        "#,
    )
    .bind(lookback)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Failed to query open positions for recovery");
            return;
        }
    };

    if rows.is_empty() {
        debug!("Startup recovery: no expired open positions found");
        return;
    }

    info!(
        count = rows.len(),
        "Startup recovery: emitting EventExpired for expired open positions"
    );

    // Group by event_id — one EventExpired per event is enough.
    let mut seen_events: HashSet<String> = HashSet::new();
    for (event_id, _symbol, end_time, resolved_up_won) in rows {
        if seen_events.insert(event_id.clone()) {
            let Some(resolved_up_won) = resolved_up_won else {
                debug!(
                    event_id = %event_id,
                    "Recovery: expired position settlement pending; waiting for official outcome",
                );
                continue;
            };
            info!(
                event_id = %event_id,
                end_time = %end_time,
                resolved_up_won = resolved_up_won,
                "Recovery: emitting EventExpired",
            );
            let _ = tx.send(MarketUpdate::EventExpired {
                event_id: Arc::from(event_id.as_str()),
                end_time,
                resolved_up_won: Some(resolved_up_won),
            });
        }
    }
}

/// On startup, find events that have NOT yet expired but already have open
/// positions from a previous runner instance. Re-inject them into the scanner's
/// `tracked` map and emit `EventDiscovered` so the strategy can manage them
/// through to settlement.
async fn recover_pending_open_positions(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    pool: &PgPool,
    tracked: &mut HashMap<String, TrackedEvent>,
    subscribed_tokens: &mut HashSet<String>,
    quote_handles: &mut Vec<JoinHandle<()>>,
) {
    let lookback = Utc::now() - Duration::hours(RECOVERY_LOOKBACK_HOURS);

    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        DateTime<Utc>,
        Option<Decimal>,
    )> = match sqlx::query_as(
        r#"
            SELECT DISTINCT
                f.event_id,
                f.symbol,
                m.raw_market->'markets'->0->>'clobTokenIds' AS clob_token_ids_json,
                f.token_id,
                f.market_side,
                m.end_time,
                m.price_to_beat
            FROM strategy_runtime_fills f
            JOIN pm_market_metadata m ON m.market_slug = f.event_id
            WHERE f.fill_side = 'BUY'
              AND f.fill_timestamp >= $1
              AND m.end_time > NOW()
              AND NOT EXISTS (
                  SELECT 1 FROM strategy_runtime_fills s2
                  WHERE s2.token_id = f.token_id
                    AND s2.fill_side = 'SELL'
              )
            "#,
    )
    .bind(lookback)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Failed to query pending open positions for recovery");
            return;
        }
    };

    if rows.is_empty() {
        debug!("Startup recovery: no pending (not-yet-expired) open positions found");
        return;
    }

    info!(
        count = rows.len(),
        "Startup recovery: re-injecting pending open positions into scanner"
    );

    let mut new_tokens: Vec<U256> = Vec::new();
    let mut seen_events: HashSet<String> = HashSet::new();
    let recovery_stop_at = rows
        .iter()
        .map(|(_, _, _, _, _, end_time, _)| *end_time)
        .max()
        .map(quote_feed_deadline);

    for (event_id, symbol, clob_json, _token_id, _market_side, end_time, price_to_beat) in rows {
        if !seen_events.insert(event_id.clone()) {
            continue;
        }

        let (up_token, down_token) = match parse_clob_token_pair(&clob_json) {
            Some(pair) => pair,
            None => {
                warn!(event_id = %event_id, "Recovery: cannot parse clobTokenIds, skipping");
                continue;
            }
        };

        let Some(up_asset_id) = parse_token_id(&up_token) else {
            continue;
        };
        let Some(down_asset_id) = parse_token_id(&down_token) else {
            continue;
        };

        if subscribed_tokens.insert(up_token.clone()) {
            new_tokens.push(up_asset_id);
        }
        if subscribed_tokens.insert(down_token.clone()) {
            new_tokens.push(down_asset_id);
        }

        tracked.insert(
            event_id.clone(),
            TrackedEvent {
                end_time,
                symbol: symbol.clone(),
                price_to_beat: price_to_beat.clone(),
            },
        );

        info!(
            event_id = %event_id,
            symbol = %symbol,
            end_time = %end_time,
            "Recovery: re-emitting EventDiscovered for pending position",
        );

        let _ = tx.send(MarketUpdate::EventDiscovered {
            event_id: Arc::from(event_id.as_str()),
            symbol: Arc::from(symbol.as_str()),
            up_token: Arc::from(up_token.as_str()),
            down_token: Arc::from(down_token.as_str()),
            end_time,
            window_secs: 300,
            price_to_beat,
            resolved_up_won: None,
        });
    }

    if !new_tokens.is_empty() {
        info!(
            tokens = new_tokens.len(),
            "Recovery: subscribing to quote feeds for pending positions"
        );
        quote_handles.push(spawn_clob_ws_quote_feed_until(
            tx.clone(),
            new_tokens,
            recovery_stop_at,
        ));
    }
}

fn extend_quote_feed_stop_at(
    current: Option<DateTime<Utc>>,
    event_end_time: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let deadline = quote_feed_deadline(event_end_time);
    Some(current.map_or(deadline, |existing| existing.max(deadline)))
}

fn quote_feed_deadline(event_end_time: DateTime<Utc>) -> DateTime<Utc> {
    event_end_time + Duration::seconds(QUOTE_FEED_GRACE_SECS)
}

fn parse_clob_token_pair(raw: &str) -> Option<(String, String)> {
    let parsed: Vec<String> = serde_json::from_str(raw).ok()?;
    if parsed.len() == 2 {
        Some((parsed[0].clone(), parsed[1].clone()))
    } else {
        None
    }
}

async fn expire_tracked_events(
    tx: &broadcast::Sender<MarketUpdate>,
    tracked: &mut HashMap<String, TrackedEvent>,
    pool: Option<&PgPool>,
    now: DateTime<Utc>,
) {
    let expired: Vec<String> = tracked
        .iter()
        .filter(|(_, event)| event.end_time <= now)
        .map(|(event_id, _)| event_id.clone())
        .collect();

    for event_id in expired {
        let resolved_up_won = match pool {
            Some(pool) => official_event_outcome(pool, &event_id).await,
            None => None,
        };

        if pool.is_some() && resolved_up_won.is_none() {
            debug!(
                event_id = %event_id,
                "Expired event settlement pending; waiting for official Polymarket outcome",
            );
            continue;
        }

        let Some(event) = tracked.remove(&event_id) else {
            continue;
        };
        let end_time = event.end_time;
        let _ = tx.send(MarketUpdate::EventExpired {
            event_id: Arc::from(event_id.as_str()),
            end_time,
            resolved_up_won,
        });
    }
}

async fn official_event_outcome(pool: &PgPool, event_id: &str) -> Option<bool> {
    match sqlx::query_scalar::<_, bool>(
        r#"
        SELECT winner.token_id = trim(both '"' from ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0))
        FROM pm_market_metadata m
        JOIN pm_token_settlements winner
          ON winner.market_slug = m.market_slug
         AND winner.resolved
         AND winner.settled_price >= 0.99
        WHERE m.market_slug = $1
        ORDER BY winner.resolved_at DESC NULLS LAST
        LIMIT 1
        "#,
    )
    .bind(event_id)
    .fetch_optional(pool)
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                event_id,
                error = %error,
                "Failed to load official settlement outcome",
            );
            None
        }
    }
}

fn should_refresh_sports_catalog(
    now: DateTime<Utc>,
    last_refresh: Option<DateTime<Utc>>,
    persistence_enabled: bool,
    capture_sports_catalog: bool,
) -> bool {
    capture_sports_catalog
        && persistence_enabled
        && last_refresh
            .map(|ts| now - ts >= Duration::seconds(SPORTS_DISCOVERY_REFRESH_SECS))
            .unwrap_or(true)
}

async fn refresh_crypto_catalog(
    _client: &GammaClient,
    pool: &PgPool,
    reference_prices: &ReferencePriceRegistry,
    symbols: &[String],
    now: DateTime<Utc>,
    lookahead_minutes: i64,
) -> usize {
    let request = crypto_markets_request(now, lookahead_minutes);
    match fetch_markets(&request, CRYPTO_DISCOVERY_MAX_MARKETS).await {
        Ok(markets) => {
            let discovered =
                discover_crypto_catalog_markets(&markets, symbols, reference_prices, now).await;
            for market in &discovered {
                persist_discovered_crypto_market(Some(pool), market).await;
            }
            discovered.len()
        }
        Err(error) => {
            warn!(error = %error, "Gamma markets keyset query failed during catalog refresh");
            0
        }
    }
}

fn crypto_markets_request(now: DateTime<Utc>, lookahead_minutes: i64) -> MarketsRequest {
    let mut request = MarketsRequest::default();
    request.end_date_min = Some(now - Duration::minutes(1));
    request.end_date_max = Some(now + Duration::minutes(lookahead_minutes));
    request.closed = Some(false);
    request.limit = Some(CRYPTO_DISCOVERY_PAGE_LIMIT);
    request
}

async fn refresh_sports_catalog(client: &GammaClient, pool: Option<&PgPool>) {
    let Some(pool) = pool else {
        return;
    };

    match discover_sports_markets(client, SPORTS_DISCOVERY_LIMIT).await {
        Ok(markets) => {
            if markets.is_empty() {
                debug!("Sports discovery refresh returned no active markets");
                return;
            }

            info!(
                discovered = markets.len(),
                "Refreshing sports market catalog",
            );

            for market in markets {
                upsert_market_catalog(
                    pool,
                    &market.descriptor,
                    market.raw_event.clone(),
                    market.raw_market.clone(),
                )
                .await;
            }
        }
        Err(error) => {
            warn!(error = %error, "Sports discovery refresh failed");
        }
    }
}

async fn persist_discovered_crypto_market(pool: Option<&PgPool>, market: &DiscoveredCryptoMarket) {
    let Some(pool) = pool else {
        return;
    };

    upsert_market_catalog(
        pool,
        &market.descriptor,
        market.raw_event.clone(),
        market.raw_market.clone(),
    )
    .await;

    upsert_market_metadata(
        pool,
        &market.compatibility_event_id,
        &market.symbol,
        &market.up_token,
        &market.down_token,
        market.descriptor.start_time.clone(),
        market.end_time.clone(),
        market.price_to_beat.clone(),
    )
    .await;
}

fn parse_token_id(token_id: &str) -> Option<U256> {
    U256::from_str(token_id).ok()
}

async fn upsert_market_metadata(
    pool: &PgPool,
    market_slug: &str,
    symbol: &str,
    up_token: &str,
    down_token: &str,
    start_time: Option<DateTime<Utc>>,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
) {
    let raw_market = serde_json::json!({
        "eventStartTime": start_time.as_ref().map(DateTime::to_rfc3339),
        "endDate": end_time.to_rfc3339(),
        "markets": [{
            "clobTokenIds": serde_json::json!([up_token, down_token]).to_string()
        }]
    });

    let result = sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (
            market_slug, symbol, start_time, end_time, price_to_beat, raw_market
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (market_slug) DO UPDATE
            SET symbol        = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
                start_time    = COALESCE(pm_market_metadata.start_time, EXCLUDED.start_time),
                end_time      = EXCLUDED.end_time,
                price_to_beat = COALESCE(EXCLUDED.price_to_beat, pm_market_metadata.price_to_beat),
                raw_market    = EXCLUDED.raw_market,
                updated_at    = NOW()
        "#,
    )
    .bind(market_slug)
    .bind(symbol)
    .bind(start_time)
    .bind(end_time)
    .bind(price_to_beat)
    .bind(raw_market)
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            market_slug,
            error = %e,
            "Failed to upsert market metadata"
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use polymarket_client_sdk::ToQueryParams;

    use crate::gamma_keyset::markets_keyset_url;

    use super::{
        crypto_markets_request, extend_quote_feed_stop_at, parse_token_id, quote_feed_deadline,
        should_refresh_sports_catalog, MarketDiscoveryCollectorConfig,
    };

    #[test]
    fn token_id_parser_accepts_decimal_strings() {
        assert!(parse_token_id(
            "27239049953613250678046988034203198692578441444398010699401021233149338414941"
        )
        .is_some());
        assert!(parse_token_id("not-a-token").is_none());
    }

    #[test]
    fn market_discovery_config_fills_safe_defaults() {
        let config = MarketDiscoveryCollectorConfig {
            symbols: Vec::new(),
            refresh_interval_secs: 0,
            lookahead_minutes: 0,
            capture_sports_catalog: false,
        }
        .with_safe_defaults();

        assert_eq!(
            config.symbols,
            vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string()
            ]
        );
        assert_eq!(config.refresh_interval_secs, super::SCAN_INTERVAL_SECS);
        assert_eq!(
            config.lookahead_minutes,
            super::DEFAULT_DISCOVERY_LOOKAHEAD_MINUTES
        );
    }

    #[test]
    fn sports_refresh_requires_pool_and_interval() {
        let now = Utc.with_ymd_and_hms(2026, 4, 6, 0, 5, 0).unwrap();
        assert!(!should_refresh_sports_catalog(now, None, false, true));
        assert!(!should_refresh_sports_catalog(now, None, true, false));
        assert!(should_refresh_sports_catalog(now, None, true, true));
        assert!(!should_refresh_sports_catalog(
            now,
            Some(now - Duration::seconds(10)),
            true,
            true
        ));
        assert!(should_refresh_sports_catalog(
            now,
            Some(now - Duration::seconds(600)),
            true,
            true
        ));
    }

    #[test]
    fn crypto_catalog_request_uses_gamma_keyset_pagination() {
        let now = Utc.with_ymd_and_hms(2026, 5, 17, 13, 30, 0).unwrap();
        let request = crypto_markets_request(now, 20);
        let query = request.query_params(None);

        assert_eq!(
            markets_keyset_url(),
            "https://gamma-api.polymarket.com/markets/keyset"
        );
        assert_eq!(request.offset, None);
        assert!(!query.contains("offset="));
        assert!(query.contains("closed=false"));
        assert!(query.contains("limit=100"));
    }

    #[test]
    fn quote_feed_deadline_tracks_latest_event_end_with_grace() {
        let first_end = Utc.with_ymd_and_hms(2026, 4, 6, 0, 5, 0).unwrap();
        let second_end = Utc.with_ymd_and_hms(2026, 4, 6, 0, 6, 0).unwrap();

        let deadline = extend_quote_feed_stop_at(None, first_end);
        assert_eq!(
            deadline,
            Some(first_end + Duration::seconds(super::QUOTE_FEED_GRACE_SECS))
        );

        let extended = extend_quote_feed_stop_at(deadline, second_end);
        assert_eq!(extended, Some(quote_feed_deadline(second_end)));

        let unchanged = extend_quote_feed_stop_at(extended, first_end);
        assert_eq!(unchanged, extended);
    }
}
