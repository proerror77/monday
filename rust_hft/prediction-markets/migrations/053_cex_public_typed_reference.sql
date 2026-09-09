-- Preserve the canonical typed reference and its coverage claim alongside the
-- original public wire payload. Existing rows remain readable with NULLs.

ALTER TABLE cex_public_market_ticks
    ADD COLUMN IF NOT EXISTS typed_reference JSONB,
    ADD COLUMN IF NOT EXISTS coverage TEXT;
