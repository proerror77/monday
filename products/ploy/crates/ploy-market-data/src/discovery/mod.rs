#[cfg(feature = "live")]
pub mod crypto;
#[cfg(feature = "live")]
pub mod sports;
#[cfg(feature = "live")]
pub mod types;

#[cfg(feature = "live")]
use serde_json::Value;
#[cfg(feature = "live")]
use sqlx::PgPool;
#[cfg(feature = "live")]
use tracing::warn;

#[cfg(feature = "live")]
use self::types::MarketDescriptor;

#[cfg(feature = "live")]
pub async fn upsert_market_catalog(
    pool: &PgPool,
    descriptor: &MarketDescriptor,
    raw_event: Option<Value>,
    raw_market: Value,
) {
    let token_ids = serde_json::to_value(&descriptor.token_ids)
        .expect("serializing token ids for market catalog cannot fail");

    let result = sqlx::query(
        r#"
        INSERT INTO pm_market_catalog (
            market_id,
            event_id,
            event_slug,
            market_slug,
            title,
            market_family,
            market_semantics,
            strategy_symbol,
            reference_symbol,
            settlement_source,
            league,
            sport,
            start_time,
            end_time,
            token_ids,
            home_team,
            away_team,
            active,
            accepting_orders,
            raw_event,
            raw_market,
            updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15::jsonb, $16, $17, $18, $19,
            $20::jsonb, $21::jsonb, NOW()
        )
        ON CONFLICT (market_id) DO UPDATE
        SET
            event_id = EXCLUDED.event_id,
            event_slug = EXCLUDED.event_slug,
            market_slug = EXCLUDED.market_slug,
            title = EXCLUDED.title,
            market_family = EXCLUDED.market_family,
            market_semantics = EXCLUDED.market_semantics,
            strategy_symbol = EXCLUDED.strategy_symbol,
            reference_symbol = EXCLUDED.reference_symbol,
            settlement_source = EXCLUDED.settlement_source,
            league = EXCLUDED.league,
            sport = EXCLUDED.sport,
            start_time = EXCLUDED.start_time,
            end_time = EXCLUDED.end_time,
            token_ids = EXCLUDED.token_ids,
            home_team = EXCLUDED.home_team,
            away_team = EXCLUDED.away_team,
            active = EXCLUDED.active,
            accepting_orders = EXCLUDED.accepting_orders,
            raw_event = COALESCE(EXCLUDED.raw_event, pm_market_catalog.raw_event),
            raw_market = EXCLUDED.raw_market,
            updated_at = NOW()
        "#,
    )
    .bind(&descriptor.market_id)
    .bind(&descriptor.event_id)
    .bind(&descriptor.event_slug)
    .bind(&descriptor.market_slug)
    .bind(&descriptor.title)
    .bind(descriptor.market_family.as_str())
    .bind(descriptor.market_semantics.as_str())
    .bind(&descriptor.strategy_symbol)
    .bind(&descriptor.reference_symbol)
    .bind(descriptor.settlement_source.as_str())
    .bind(&descriptor.league)
    .bind(&descriptor.sport)
    .bind(descriptor.start_time)
    .bind(descriptor.end_time)
    .bind(token_ids)
    .bind(&descriptor.home_team)
    .bind(&descriptor.away_team)
    .bind(descriptor.active)
    .bind(descriptor.accepting_orders)
    .bind(raw_event)
    .bind(raw_market)
    .execute(pool)
    .await;

    if let Err(error) = result {
        warn!(
            market_id = %descriptor.market_id,
            error = %error,
            "Failed to upsert pm_market_catalog"
        );
    }
}
