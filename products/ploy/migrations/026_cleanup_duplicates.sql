-- Clean up duplicate rows before creating unique indexes

-- Delete duplicate clob_quote_ticks, keeping only the latest received_at per (token_id, second)
DELETE FROM clob_quote_ticks
WHERE id IN (
    SELECT id
    FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY token_id, date_trunc('second', received_at AT TIME ZONE 'UTC')
                   ORDER BY received_at DESC
               ) AS rn
        FROM clob_quote_ticks
    ) t
    WHERE rn > 1
);

-- Delete duplicate binance_price_ticks, keeping only the latest trade_time per (symbol, second)
DELETE FROM binance_price_ticks
WHERE id IN (
    SELECT id
    FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY symbol, date_trunc('second', trade_time AT TIME ZONE 'UTC')
                   ORDER BY trade_time DESC
               ) AS rn
        FROM binance_price_ticks
    ) t
    WHERE rn > 1
);
