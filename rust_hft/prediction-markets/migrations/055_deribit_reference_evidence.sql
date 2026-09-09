-- Canonical typed Deribit IV/Greeks evidence. Existing rows remain historical
-- and are not reinterpreted when their source unit/availability is unknown.
-- `source_iv_unit` describes the raw API fields; `iv_unit` describes the
-- numeric mark/bid/ask columns stored by the canonical sink.

-- The original collectors predated repository migrations, so clean databases
-- may not have either table yet. Define the columns used by the old sink and
-- the typed sink before applying the additive evidence columns below. Existing
-- tables are left untouched by CREATE TABLE IF NOT EXISTS.
CREATE TABLE IF NOT EXISTS deribit_iv_ticks (
    id BIGSERIAL PRIMARY KEY,
    currency TEXT NOT NULL,
    instrument_name TEXT NOT NULL,
    creation_ts TIMESTAMPTZ NOT NULL,
    expiry_ts TIMESTAMPTZ,
    mark_iv NUMERIC,
    bid_iv NUMERIC,
    ask_iv NUMERIC,
    underlying_price NUMERIC,
    index_price NUMERIC,
    mark_price NUMERIC,
    best_bid_price NUMERIC,
    best_ask_price NUMERIC,
    open_interest NUMERIC,
    volume NUMERIC,
    payload JSONB NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL,
    UNIQUE (currency, instrument_name, creation_ts, fetched_at)
);

CREATE TABLE IF NOT EXISTS deribit_atm_greeks_ticks (
    id BIGSERIAL PRIMARY KEY,
    currency TEXT NOT NULL,
    instrument_name TEXT NOT NULL,
    source_ts TIMESTAMPTZ NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL,
    mark_iv NUMERIC,
    bid_iv NUMERIC,
    ask_iv NUMERIC,
    delta NUMERIC,
    gamma NUMERIC,
    vega NUMERIC,
    theta NUMERIC,
    rho NUMERIC,
    mark_price NUMERIC,
    underlying_price NUMERIC,
    index_price NUMERIC,
    best_bid_price NUMERIC,
    best_ask_price NUMERIC,
    open_interest NUMERIC,
    raw JSONB NOT NULL,
    UNIQUE (currency, instrument_name, source_ts)
);

ALTER TABLE deribit_iv_ticks
    ADD COLUMN IF NOT EXISTS strike NUMERIC,
    ADD COLUMN IF NOT EXISTS option_type TEXT,
    ADD COLUMN IF NOT EXISTS source_iv_unit TEXT,
    ADD COLUMN IF NOT EXISTS iv_unit TEXT,
    ADD COLUMN IF NOT EXISTS source_timestamp_ms BIGINT,
    ADD COLUMN IF NOT EXISTS canonical_observation JSONB;

ALTER TABLE deribit_atm_greeks_ticks
    ADD COLUMN IF NOT EXISTS strike NUMERIC,
    ADD COLUMN IF NOT EXISTS option_type TEXT,
    ADD COLUMN IF NOT EXISTS source_iv_unit TEXT,
    ADD COLUMN IF NOT EXISTS iv_unit TEXT,
    ADD COLUMN IF NOT EXISTS canonical_observation JSONB;

CREATE INDEX IF NOT EXISTS idx_deribit_iv_ticks_currency_fetched_at
    ON deribit_iv_ticks (currency, fetched_at DESC);
CREATE INDEX IF NOT EXISTS idx_deribit_greeks_ticks_currency_fetched_at
    ON deribit_atm_greeks_ticks (currency, fetched_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS uq_deribit_iv_ticks_typed_identity
    ON deribit_iv_ticks (currency, instrument_name, source_timestamp_ms, fetched_at)
    WHERE source_timestamp_ms IS NOT NULL
      AND source_iv_unit = 'percent_points'
      AND iv_unit = 'decimal_fraction';

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ploy') THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.deribit_iv_ticks TO ploy';
        EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.deribit_atm_greeks_ticks TO ploy';
        IF to_regclass('public.deribit_iv_ticks_id_seq') IS NOT NULL THEN
            EXECUTE 'GRANT USAGE, SELECT ON SEQUENCE public.deribit_iv_ticks_id_seq TO ploy';
        END IF;
        IF to_regclass('public.deribit_atm_greeks_ticks_id_seq') IS NOT NULL THEN
            EXECUTE 'GRANT USAGE, SELECT ON SEQUENCE public.deribit_atm_greeks_ticks_id_seq TO ploy';
        END IF;
    END IF;
END $$;
