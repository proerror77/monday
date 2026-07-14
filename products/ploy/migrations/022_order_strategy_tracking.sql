-- Migration 022: Add strategy_id and fee tracking to orders
-- Enables post-trade analysis by strategy and fee reconciliation

ALTER TABLE orders ADD COLUMN IF NOT EXISTS strategy_id TEXT;
ALTER TABLE orders ADD COLUMN IF NOT EXISTS fee DECIMAL(18,8);

CREATE INDEX IF NOT EXISTS idx_orders_strategy_time ON orders(strategy_id, created_at DESC);
