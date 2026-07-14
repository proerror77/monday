use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use ploy_market_contracts::MarketUpdate;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

const SPORTS_WS_ENDPOINT: &str = "wss://sports-api.polymarket.com/ws";
const SPORTS_WS_SOURCE: &str = "polymarket_sports_ws";
const RECONNECT_DELAY_SECS: u64 = 5;
const MAX_RECONNECT_DELAY_SECS: u64 = 60;
const DESCRIPTOR_REFRESH_SECS: i64 = 60;

#[derive(Debug, Clone, Default)]
struct SportsDescriptor {
    league: Option<String>,
    home_team: Option<String>,
    away_team: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct SportsDescriptorCache {
    by_slug: HashMap<String, SportsDescriptor>,
    last_refresh: Option<DateTime<Utc>>,
}

impl SportsDescriptorCache {
    async fn refresh_if_stale(&mut self, pool: Option<&PgPool>, now: DateTime<Utc>) {
        let Some(pool) = pool else {
            return;
        };
        if self
            .last_refresh
            .map(|ts| now - ts < chrono::Duration::seconds(DESCRIPTOR_REFRESH_SECS))
            .unwrap_or(false)
        {
            return;
        }

        let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (LOWER(COALESCE(event_slug, market_slug)))
                LOWER(COALESCE(event_slug, market_slug)) AS join_slug,
                league,
                home_team,
                away_team
            FROM pm_market_catalog
            WHERE market_family = 'sports'
              AND COALESCE(event_slug, market_slug) IS NOT NULL
            ORDER BY LOWER(COALESCE(event_slug, market_slug)), updated_at DESC
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        self.by_slug = rows
            .into_iter()
            .map(|(slug, league, home_team, away_team)| {
                (
                    slug,
                    SportsDescriptor {
                        league,
                        home_team,
                        away_team,
                    },
                )
            })
            .collect();
        self.last_refresh = Some(now);
    }

    fn get(&self, slug: &str) -> Option<&SportsDescriptor> {
        self.by_slug.get(&slug.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone)]
struct ParsedSportsState {
    update: MarketUpdate,
    raw_message: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SportsWsPayload {
    #[serde(rename = "gameId")]
    game_id: Value,
    league_abbreviation: Option<String>,
    slug: Option<String>,
    home_team: Option<String>,
    away_team: Option<String>,
    status: Option<String>,
    score: Option<String>,
    period: Option<String>,
    elapsed: Option<String>,
    live: Option<bool>,
    ended: Option<bool>,
    #[serde(rename = "finished_timestamp")]
    finished_timestamp: Option<DateTime<Utc>>,
}

pub fn spawn_sports_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut descriptors = SportsDescriptorCache::default();
        let mut reconnect_delay = Duration::from_secs(RECONNECT_DELAY_SECS);

        loop {
            descriptors
                .refresh_if_stale(pool.as_ref(), Utc::now())
                .await;

            match connect_async(SPORTS_WS_ENDPOINT).await {
                Ok((mut stream, _)) => {
                    info!(
                        endpoint = SPORTS_WS_ENDPOINT,
                        "Connected to sports websocket"
                    );

                    loop {
                        descriptors
                            .refresh_if_stale(pool.as_ref(), Utc::now())
                            .await;

                        match stream.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if text == "ping" {
                                    if stream.send(Message::Text("pong".into())).await.is_err() {
                                        warn!("Failed to respond to sports websocket ping");
                                        break;
                                    }
                                    continue;
                                }

                                match parse_message_text(&text, Utc::now(), &descriptors) {
                                    Ok(Some(parsed)) => {
                                        reconnect_delay = Duration::from_secs(RECONNECT_DELAY_SECS);
                                        if let Some(db) = pool.as_ref() {
                                            persist_sports_state(db, &parsed).await;
                                        }
                                        let _ = tx.send(parsed.update);
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        warn!(error = %error, message = %text, "Failed to parse sports websocket message");
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if stream.send(Message::Pong(payload)).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                debug!(?frame, "Sports websocket closed");
                                break;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                warn!(error = %error, "Sports websocket receive error");
                                break;
                            }
                            None => {
                                warn!("Sports websocket stream ended");
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    warn!(error = %error, endpoint = SPORTS_WS_ENDPOINT, "Failed to connect to sports websocket");
                }
            }

            tokio::time::sleep(reconnect_delay).await;
            reconnect_delay =
                (reconnect_delay * 2).min(Duration::from_secs(MAX_RECONNECT_DELAY_SECS));
        }
    })
}

fn parse_message_text(
    text: &str,
    received_at: DateTime<Utc>,
    descriptors: &SportsDescriptorCache,
) -> Result<Option<ParsedSportsState>, serde_json::Error> {
    if text.trim().is_empty() || text == "ping" || text == "pong" {
        return Ok(None);
    }

    let raw_message: Value = serde_json::from_str(text)?;
    let payload: SportsWsPayload = serde_json::from_value(raw_message.clone())?;
    let Some(game_id) = normalize_game_id(&payload.game_id) else {
        return Ok(None);
    };
    let Some(slug) = non_empty(payload.slug) else {
        return Ok(None);
    };

    let descriptor = descriptors.get(&slug);
    let league = non_empty(payload.league_abbreviation)
        .or_else(|| descriptor.and_then(|value| value.league.clone()))
        .unwrap_or_default();
    let home_team = non_empty(payload.home_team)
        .or_else(|| descriptor.and_then(|value| value.home_team.clone()))
        .unwrap_or_default();
    let away_team = non_empty(payload.away_team)
        .or_else(|| descriptor.and_then(|value| value.away_team.clone()))
        .unwrap_or_default();
    let status = non_empty(payload.status).unwrap_or_default();

    if league.is_empty() || home_team.is_empty() || away_team.is_empty() || status.is_empty() {
        return Ok(None);
    }

    Ok(Some(ParsedSportsState {
        update: MarketUpdate::SportsState {
            game_id: Arc::from(game_id.as_str()),
            league: Arc::from(league.as_str()),
            slug: Arc::from(slug.as_str()),
            home_team: Arc::from(home_team.as_str()),
            away_team: Arc::from(away_team.as_str()),
            status: Arc::from(status.as_str()),
            period: non_empty(payload.period).map(|s| Arc::from(s.as_str())),
            score: non_empty(payload.score).map(|s| Arc::from(s.as_str())),
            elapsed: non_empty(payload.elapsed).map(|s| Arc::from(s.as_str())),
            live: payload.live.unwrap_or(false),
            ended: payload.ended.unwrap_or(false),
            finished_at: payload.finished_timestamp,
            ts: received_at,
        },
        raw_message,
    }))
}

async fn persist_sports_state(pool: &PgPool, parsed: &ParsedSportsState) {
    let MarketUpdate::SportsState {
        game_id,
        league,
        slug,
        home_team,
        away_team,
        status,
        period,
        score,
        elapsed,
        live,
        ended,
        finished_at,
        ts,
    } = &parsed.update
    else {
        return;
    };

    let result = sqlx::query(
        r#"
        INSERT INTO sports_state_events (
            game_id,
            league,
            slug,
            home_team,
            away_team,
            status,
            period,
            score,
            elapsed,
            live,
            ended,
            finished_at,
            source,
            event_time,
            received_at,
            raw_message
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, NOW(), $15::jsonb
        )
        "#,
    )
    .bind(&**game_id)
    .bind(&**league)
    .bind(&**slug)
    .bind(&**home_team)
    .bind(&**away_team)
    .bind(&**status)
    .bind(period.as_deref())
    .bind(score.as_deref())
    .bind(elapsed.as_deref())
    .bind(*live)
    .bind(*ended)
    .bind(*finished_at)
    .bind(SPORTS_WS_SOURCE)
    .bind(*ts)
    .bind(&parsed.raw_message)
    .execute(pool)
    .await;

    if let Err(error) = result {
        warn!(
            game_id = %game_id,
            slug = %slug,
            error = %error,
            "Failed to persist sports_state_events row"
        );
    }
}

fn normalize_game_id(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) if !raw.trim().is_empty() => Some(raw.trim().to_string()),
        Value::Number(raw) => Some(raw.to_string()),
        _ => None,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use ploy_market_contracts::MarketUpdate;

    use super::{parse_message_text, SportsDescriptor, SportsDescriptorCache};

    #[test]
    fn fixture_messages_normalize_into_sports_state_updates() {
        let fixture = include_str!("../tests/fixtures/polymarket_sports_ws.jsonl");
        let mut updates = Vec::new();

        for (idx, line) in fixture.lines().enumerate() {
            let received_at = Utc.with_ymd_and_hms(2026, 4, 6, 0, idx as u32, 0).unwrap();
            if let Some(parsed) =
                parse_message_text(line, received_at, &SportsDescriptorCache::default())
                    .expect("fixture line should parse")
            {
                updates.push(parsed.update);
            }
        }

        assert_eq!(updates.len(), 4);
        assert!(matches!(
            &updates[0],
            MarketUpdate::SportsState { status, live, ended, .. }
                if status.as_ref() == "Scheduled" && !live && !ended
        ));
        assert!(matches!(
            &updates[1],
            MarketUpdate::SportsState { status, period, live, .. }
                if status.as_ref() == "InProgress" && period.as_deref() == Some("Q4") && *live
        ));
        assert!(matches!(
            &updates[2],
            MarketUpdate::SportsState { status, period, .. }
                if status.as_ref() == "Break" && period.as_deref() == Some("HT")
        ));
        assert!(matches!(
            &updates[3],
            MarketUpdate::SportsState { status, ended, finished_at, .. }
                if status.as_ref() == "finished" && *ended && finished_at.is_some()
        ));
    }

    #[test]
    fn descriptor_cache_fills_missing_team_metadata() {
        let mut cache = SportsDescriptorCache::default();
        cache.by_slug.insert(
            "nba-lal-bos-2026-04-06".to_string(),
            SportsDescriptor {
                league: Some("nba".to_string()),
                home_team: Some("LAL".to_string()),
                away_team: Some("BOS".to_string()),
            },
        );

        let received_at = Utc.with_ymd_and_hms(2026, 4, 6, 1, 0, 0).unwrap();
        let parsed = parse_message_text(
            r#"{"gameId":123,"leagueAbbreviation":"nba","slug":"nba-lal-bos-2026-04-06","status":"InProgress","score":"88-91","period":"Q4","elapsed":"2:10","live":true,"ended":false}"#,
            received_at,
            &cache,
        )
        .expect("message should parse")
        .expect("message should normalize");

        assert!(matches!(
            parsed.update,
            MarketUpdate::SportsState { home_team, away_team, .. }
                if home_team.as_ref() == "LAL" && away_team.as_ref() == "BOS"
        ));
    }
}
