-- Polymarket Trading Bot Schema
-- Run with: sqlx migrate run

-- Rounds tracking
CREATE TABLE IF NOT EXISTS rounds (
    id SERIAL PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    up_token_id TEXT NOT NULL,
    down_token_id TEXT NOT NULL,
    start_time TIMESTAMPTZ NOT NULL,
    end_time TIMESTAMPTZ NOT NULL,
    outcome TEXT,  -- 'UP', 'DOWN', or NULL if pending
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_rounds_slug ON rounds(slug);
CREATE INDEX IF NOT EXISTS idx_rounds_start_time ON rounds(start_time);
CREATE INDEX IF NOT EXISTS idx_rounds_end_time ON rounds(end_time);

-- Tick data for backtesting
CREATE TABLE IF NOT EXISTS ticks (
    id BIGSERIAL PRIMARY KEY,
    round_id INT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
    timestamp TIMESTAMPTZ NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
    best_bid DECIMAL(10,6),
    best_ask DECIMAL(10,6),
    bid_size DECIMAL(18,8),
    ask_size DECIMAL(18,8)
);

CREATE INDEX IF NOT EXISTS idx_ticks_round_id ON ticks(round_id);
CREATE INDEX IF NOT EXISTS idx_ticks_timestamp ON ticks(timestamp);
CREATE INDEX IF NOT EXISTS idx_ticks_round_side ON ticks(round_id, side);

-- Cycles (trading attempts)
CREATE TABLE IF NOT EXISTS cycles (
    id SERIAL PRIMARY KEY,
    round_id INT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    leg1_side TEXT CHECK (leg1_side IN ('UP', 'DOWN')),
    leg1_entry_price DECIMAL(10,6),
    leg1_shares INT,
    leg1_filled_at TIMESTAMPTZ,
    leg2_entry_price DECIMAL(10,6),
    leg2_shares INT,
    leg2_filled_at TIMESTAMPTZ,
    pnl DECIMAL(18,8),
    abort_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_cycles_round_id ON cycles(round_id);
CREATE INDEX IF NOT EXISTS idx_cycles_state ON cycles(state);
CREATE INDEX IF NOT EXISTS idx_cycles_created_at ON cycles(created_at);

-- Orders
CREATE TABLE IF NOT EXISTS orders (
    id SERIAL PRIMARY KEY,
    cycle_id INT REFERENCES cycles(id) ON DELETE SET NULL,
    leg INT NOT NULL CHECK (leg IN (1, 2)),
    client_order_id TEXT NOT NULL UNIQUE,
    exchange_order_id TEXT,
    market_side TEXT NOT NULL CHECK (market_side IN ('UP', 'DOWN')),
    order_side TEXT NOT NULL CHECK (order_side IN ('BUY', 'SELL')),
    token_id TEXT NOT NULL,
    shares INT NOT NULL,
    limit_price DECIMAL(10,6) NOT NULL,
    avg_fill_price DECIMAL(10,6),
    filled_shares INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    submitted_at TIMESTAMPTZ,
    filled_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_orders_cycle_id ON orders(cycle_id);
CREATE INDEX IF NOT EXISTS idx_orders_client_order_id ON orders(client_order_id);
CREATE INDEX IF NOT EXISTS idx_orders_status ON orders(status);
CREATE INDEX IF NOT EXISTS idx_orders_created_at ON orders(created_at);

-- Daily metrics for risk tracking
CREATE TABLE IF NOT EXISTS daily_metrics (
    date DATE PRIMARY KEY,
    total_cycles INT NOT NULL DEFAULT 0,
    completed_cycles INT NOT NULL DEFAULT 0,
    aborted_cycles INT NOT NULL DEFAULT 0,
    leg2_completions INT NOT NULL DEFAULT 0,
    total_pnl DECIMAL(18,8) NOT NULL DEFAULT 0,
    max_drawdown DECIMAL(18,8) NOT NULL DEFAULT 0,
    consecutive_failures INT NOT NULL DEFAULT 0,
    halted BOOLEAN NOT NULL DEFAULT FALSE,
    halt_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- State persistence for recovery
CREATE TABLE IF NOT EXISTS strategy_state (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),  -- Singleton
    current_state TEXT NOT NULL,
    current_round_id INT REFERENCES rounds(id),
    current_cycle_id INT REFERENCES cycles(id),
    risk_state TEXT NOT NULL DEFAULT 'NORMAL',
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Insert default state
INSERT INTO strategy_state (current_state, risk_state)
VALUES ('IDLE', 'NORMAL')
ON CONFLICT (id) DO NOTHING;

-- Dump signals (for analysis)
CREATE TABLE IF NOT EXISTS dump_signals (
    id SERIAL PRIMARY KEY,
    round_id INT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
    side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
    trigger_price DECIMAL(10,6) NOT NULL,
    reference_price DECIMAL(10,6) NOT NULL,
    drop_pct DECIMAL(10,6) NOT NULL,
    spread_bps INT NOT NULL,
    was_valid BOOLEAN NOT NULL,
    was_acted_on BOOLEAN NOT NULL DEFAULT FALSE,
    timestamp TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dump_signals_round_id ON dump_signals(round_id);
CREATE INDEX IF NOT EXISTS idx_dump_signals_timestamp ON dump_signals(timestamp);

-- Function to update updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Apply trigger to tables with updated_at
DO $$
DECLARE
    t text;
BEGIN
    FOR t IN
        SELECT table_name
        FROM information_schema.columns
        WHERE column_name = 'updated_at'
        AND table_schema = 'public'
    LOOP
        EXECUTE format('
            DROP TRIGGER IF EXISTS update_%I_updated_at ON %I;
            CREATE TRIGGER update_%I_updated_at
            BEFORE UPDATE ON %I
            FOR EACH ROW
            EXECUTE FUNCTION update_updated_at_column();
        ', t, t, t, t);
    END LOOP;
END;
$$;
