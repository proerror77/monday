-- Canonical Predict.fun REST book evidence. Existing rows remain historical
-- projections with NULL readiness/raw/full-depth fields.

ALTER TABLE predict_fun_markets
    ADD COLUMN IF NOT EXISTS raw JSONB;

ALTER TABLE predict_fun_orderbook_ticks
    ADD COLUMN IF NOT EXISTS ready BOOLEAN,
    ADD COLUMN IF NOT EXISTS yes_bids JSONB,
    ADD COLUMN IF NOT EXISTS yes_asks JSONB,
    ADD COLUMN IF NOT EXISTS no_bids JSONB,
    ADD COLUMN IF NOT EXISTS no_asks JSONB,
    ADD COLUMN IF NOT EXISTS raw JSONB;
