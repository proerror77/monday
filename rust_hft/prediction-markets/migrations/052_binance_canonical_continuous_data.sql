-- Canonical Binance continuous market-data identity and sequence provenance.
-- Existing rows are intentionally left NULL/unknown; no historical venue or
-- receive timestamp is inferred from the old symbol-only rows.

ALTER TABLE binance_price_ticks
    ADD COLUMN IF NOT EXISTS market_type TEXT,
    ADD COLUMN IF NOT EXISTS venue TEXT,
    ADD COLUMN IF NOT EXISTS trade_id BIGINT,
    ADD COLUMN IF NOT EXISTS event_time TIMESTAMPTZ;

ALTER TABLE binance_price_ticks
    DROP CONSTRAINT IF EXISTS binance_price_ticks_market_type_check;
ALTER TABLE binance_price_ticks
    ADD CONSTRAINT binance_price_ticks_market_type_check
    CHECK (market_type IS NULL OR market_type IN ('spot', 'usd_m'));
ALTER TABLE binance_price_ticks
    DROP CONSTRAINT IF EXISTS binance_price_ticks_venue_check;
ALTER TABLE binance_price_ticks
    ADD CONSTRAINT binance_price_ticks_venue_check
    CHECK (venue IS NULL OR venue IN ('binance', 'binance_futures'));

-- The previous symbol/second index discarded continuous raw trades. Keep old
-- rows as-is and deduplicate only new canonical rows by their true identity.
DROP INDEX IF EXISTS uq_binance_price_ticks_symbol_second;
CREATE UNIQUE INDEX IF NOT EXISTS uq_binance_price_ticks_canonical_trade
    ON binance_price_ticks (venue, market_type, symbol, trade_id)
    WHERE venue IS NOT NULL AND market_type IS NOT NULL AND trade_id IS NOT NULL;

ALTER TABLE binance_agg_trade_ticks
    ADD COLUMN IF NOT EXISTS market_type TEXT,
    ADD COLUMN IF NOT EXISTS venue TEXT;

ALTER TABLE binance_agg_trade_ticks
    DROP CONSTRAINT IF EXISTS binance_agg_trade_ticks_market_type_check;
ALTER TABLE binance_agg_trade_ticks
    ADD CONSTRAINT binance_agg_trade_ticks_market_type_check
    CHECK (market_type IS NULL OR market_type IN ('spot', 'usd_m'));
ALTER TABLE binance_agg_trade_ticks
    DROP CONSTRAINT IF EXISTS binance_agg_trade_ticks_venue_check;
ALTER TABLE binance_agg_trade_ticks
    ADD CONSTRAINT binance_agg_trade_ticks_venue_check
    CHECK (venue IS NULL OR venue IN ('binance', 'binance_futures'));

-- The old unique key could collide when Spot and USD-M use the same symbol and
-- numeric aggregate ID. New canonical rows use the full venue identity.
ALTER TABLE binance_agg_trade_ticks
    DROP CONSTRAINT IF EXISTS binance_agg_trade_ticks_symbol_agg_trade_id_key;
CREATE UNIQUE INDEX IF NOT EXISTS uq_binance_agg_trade_ticks_canonical_identity
    ON binance_agg_trade_ticks (venue, market_type, symbol, agg_trade_id)
    WHERE venue IS NOT NULL AND market_type IS NOT NULL;

ALTER TABLE binance_lob_ticks
    ADD COLUMN IF NOT EXISTS market_type TEXT,
    ADD COLUMN IF NOT EXISTS venue TEXT,
    ADD COLUMN IF NOT EXISTS first_sequence BIGINT,
    ADD COLUMN IF NOT EXISTS previous_sequence BIGINT,
    ADD COLUMN IF NOT EXISTS depth_mode TEXT;

-- Partial-depth snapshots and REST seeds have a real local receive timestamp
-- but no Binance exchange event clock. Keep that source clock explicitly NULL
-- instead of making local arrival look like exchange time.
ALTER TABLE binance_lob_ticks
    ALTER COLUMN event_time DROP NOT NULL;

ALTER TABLE binance_lob_ticks
    DROP CONSTRAINT IF EXISTS binance_lob_ticks_market_type_check;
ALTER TABLE binance_lob_ticks
    ADD CONSTRAINT binance_lob_ticks_market_type_check
    CHECK (market_type IS NULL OR market_type IN ('spot', 'usd_m'));
ALTER TABLE binance_lob_ticks
    DROP CONSTRAINT IF EXISTS binance_lob_ticks_venue_check;
ALTER TABLE binance_lob_ticks
    ADD CONSTRAINT binance_lob_ticks_venue_check
    CHECK (venue IS NULL OR venue IN ('binance', 'binance_futures'));
ALTER TABLE binance_lob_ticks
    DROP CONSTRAINT IF EXISTS binance_lob_ticks_depth_mode_check;
ALTER TABLE binance_lob_ticks
    ADD CONSTRAINT binance_lob_ticks_depth_mode_check
    CHECK (depth_mode IS NULL OR depth_mode IN ('partial', 'diff'));

CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_identity_time
    ON binance_price_ticks (venue, market_type, symbol, trade_time DESC);
CREATE INDEX IF NOT EXISTS idx_binance_agg_trade_ticks_identity_time
    ON binance_agg_trade_ticks (venue, market_type, symbol, trade_time DESC);
CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_identity_time
    ON binance_lob_ticks (venue, market_type, symbol, event_time DESC);
