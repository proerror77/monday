use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;

pub async fn check_database(db_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await?;

    println!("=== Database Data Completeness Check ===\n");

    let tables = vec![
        "sync_records",
        "binance_price_ticks",
        "clob_quote_ticks",
        "pm_market_metadata",
        "pm_market_catalog",
        "reference_price_ticks",
        "sports_state_events",
        "binance_lob_ticks",
    ];

    for table in &tables {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = $1
            )",
        )
        .bind(table)
        .fetch_one(&pool)
        .await?;

        println!(
            "Table '{}': {}",
            table,
            if exists { "EXISTS" } else { "MISSING" }
        );
    }

    println!("\n=== Data Range Analysis ===\n");

    let symbols = vec![
        "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT",
    ];

    println!("--- binance_price_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(trade_time), MAX(trade_time) FROM binance_price_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            println!(
                "  {}: {} rows, {} to {}",
                symbol,
                count,
                min_ts
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
                max_ts
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "N/A".to_string())
            );
        }
    }

    println!("\n--- clob_quote_ticks ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT COUNT(*), MIN(received_at), MAX(received_at) FROM clob_quote_ticks")
            .fetch_optional(&pool)
            .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!(
            "  Total: {} rows, {} to {}",
            count,
            min_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            max_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string())
        );
    }

    println!("\n--- pm_market_metadata ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT COUNT(*), MIN(start_time), MAX(end_time) FROM pm_market_metadata")
            .fetch_optional(&pool)
            .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!(
            "  Total: {} markets, {} to {}",
            count,
            min_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            max_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string())
        );
    }

    println!("\n--- binance_lob_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(event_time), MAX(event_time) FROM binance_lob_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            if count > 0 {
                println!(
                    "  {}: {} rows, {} to {}",
                    symbol,
                    count,
                    min_ts
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "N/A".to_string()),
                    max_ts
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "N/A".to_string())
                );
            }
        }
    }

    println!("\n=== Recommendation ===");
    println!("Based on the data ranges above, choose a backtest period where:");
    println!("1. All required symbols have continuous data");
    println!("2. pm_market_metadata has sufficient markets");
    println!("3. clob_quote_ticks has good coverage");

    Ok(())
}
