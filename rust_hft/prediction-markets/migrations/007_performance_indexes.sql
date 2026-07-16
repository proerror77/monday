-- Migration 007: Performance Optimization Indexes (drift-safe)
--
-- This migration creates performance indexes using best-effort schema detection
-- so mixed-version databases can continue migrating forward.

DO $$
BEGIN
    -- =========================================================================
    -- Orders
    -- =========================================================================
    IF to_regclass('public.orders') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'orders'
              AND column_name = 'client_order_id'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_client_order_id ON orders(client_order_id) WHERE client_order_id IS NOT NULL';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'orders'
              AND column_name = 'status'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'orders'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_status_created ON orders(status, created_at DESC) WHERE status IN (''pending'', ''submitted'', ''partial'', ''PENDING'', ''SUBMITTED'', ''PARTIAL'')';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'orders'
              AND column_name = 'cycle_id'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'orders'
              AND column_name = 'created_at'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'orders'
                  AND column_name = 'leg'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg ON orders(cycle_id, leg, created_at DESC) WHERE cycle_id IS NOT NULL';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'orders'
                  AND column_name = 'leg_number'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_orders_cycle_leg ON orders(cycle_id, leg_number, created_at DESC) WHERE cycle_id IS NOT NULL';
            END IF;
        END IF;
    END IF;

    -- =========================================================================
    -- Positions
    -- =========================================================================
    IF to_regclass('public.positions') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'status'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'opened_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_status_opened ON positions(status, opened_at DESC) WHERE upper(status) = ''OPEN''';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'token_id'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'status'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'opened_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_token_status ON positions(token_id, status, opened_at DESC)';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'event_id'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'status'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'opened_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_event_status ON positions(event_id, status, opened_at DESC) WHERE event_id IS NOT NULL';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'strategy_id'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'closed_at'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'positions'
              AND column_name = 'status'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_positions_strategy_closed ON positions(strategy_id, closed_at DESC) WHERE strategy_id IS NOT NULL AND upper(status) = ''CLOSED''';
        END IF;
    END IF;

    -- =========================================================================
    -- Reconciliation + discrepancies
    -- =========================================================================
    IF to_regclass('public.position_reconciliation_log') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'position_reconciliation_log'
              AND column_name = 'timestamp'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created ON position_reconciliation_log(timestamp DESC)';
        ELSIF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'position_reconciliation_log'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_reconciliation_log_created ON position_reconciliation_log(created_at DESC)';
        END IF;
    END IF;

    IF to_regclass('public.position_discrepancies') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'position_discrepancies'
              AND column_name = 'created_at'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'position_discrepancies'
                  AND column_name = 'resolved'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_unresolved ON position_discrepancies(created_at DESC) WHERE resolved = FALSE';
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_severity ON position_discrepancies(severity, created_at DESC) WHERE resolved = FALSE';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'position_discrepancies'
                  AND column_name = 'resolved_at'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_unresolved ON position_discrepancies(created_at DESC) WHERE resolved_at IS NULL';
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_discrepancies_severity ON position_discrepancies(severity, created_at DESC) WHERE resolved_at IS NULL';
            END IF;
        END IF;
    END IF;

    -- =========================================================================
    -- Idempotency + state transitions
    -- =========================================================================
    IF to_regclass('public.order_idempotency') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'order_idempotency'
              AND column_name = 'idempotency_key'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'order_idempotency'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_idempotency_key_created ON order_idempotency(idempotency_key, created_at DESC)';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'order_idempotency'
              AND column_name = 'expires_at'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'order_idempotency'
              AND column_name = 'status'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_idempotency_expires ON order_idempotency(expires_at) WHERE status IN (''pending'', ''failed'', ''PENDING'', ''FAILED'')';
        END IF;
    END IF;

    IF to_regclass('public.state_transitions') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'state_transitions'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_state_transitions_created ON state_transitions(created_at DESC)';
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'state_transitions'
                  AND column_name = 'success'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_state_transitions_failed ON state_transitions(created_at DESC) WHERE success = FALSE';
            END IF;
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'state_transitions'
                  AND column_name = 'cycle_id'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_state_transitions_cycle ON state_transitions(cycle_id, created_at DESC) WHERE cycle_id IS NOT NULL';
            END IF;
        END IF;
    END IF;

    -- =========================================================================
    -- Nonce usage
    -- =========================================================================
    IF to_regclass('public.nonce_usage') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'nonce_usage'
              AND column_name = 'wallet_address'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'nonce_usage'
              AND column_name = 'released_at'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'nonce_usage'
                  AND column_name = 'allocated_at'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, allocated_at DESC) WHERE released_at IS NULL';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'nonce_usage'
                  AND column_name = 'used_at'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_active ON nonce_usage(wallet_address, used_at DESC) WHERE released_at IS NULL';
            END IF;
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'nonce_usage'
              AND column_name = 'released_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_nonce_usage_released ON nonce_usage(released_at) WHERE released_at IS NOT NULL';
        END IF;
    END IF;

    -- =========================================================================
    -- Fills + balances
    -- =========================================================================
    IF to_regclass('public.fills') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'fills'
              AND column_name = 'position_id'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'fills'
                  AND column_name = 'timestamp'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, timestamp DESC)';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'fills'
                  AND column_name = 'filled_at'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_position_time ON fills(position_id, filled_at DESC)';
            END IF;
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'fills'
              AND column_name = 'order_id'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'fills'
                  AND column_name = 'timestamp'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, timestamp DESC)';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'fills'
                  AND column_name = 'filled_at'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_fills_order_time ON fills(order_id, filled_at DESC)';
            END IF;
        END IF;
    END IF;

    IF to_regclass('public.balance_snapshots') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'balance_snapshots'
              AND column_name = 'wallet_address'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'balance_snapshots'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(wallet_address, created_at DESC)';
        ELSIF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'balance_snapshots'
              AND column_name = 'timestamp'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(timestamp DESC)';
        ELSIF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'balance_snapshots'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_balance_snapshots_latest ON balance_snapshots(created_at DESC)';
        END IF;
    END IF;

    -- =========================================================================
    -- DLQ + component health + system events
    -- =========================================================================
    IF to_regclass('public.dead_letter_queue') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'dead_letter_queue'
              AND column_name = 'created_at'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'dead_letter_queue'
              AND column_name = 'status'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'dead_letter_queue'
              AND column_name = 'retry_count'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'dead_letter_queue'
              AND column_name = 'max_retries'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_dlq_pending ON dead_letter_queue(created_at) WHERE status = ''pending'' AND retry_count < max_retries';
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_dlq_failed ON dead_letter_queue(created_at DESC) WHERE status = ''failed''';
        END IF;
    END IF;

    IF to_regclass('public.component_heartbeats') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'component_heartbeats'
              AND column_name = 'last_heartbeat'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'component_heartbeats'
                  AND column_name = 'component_name'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component_name, last_heartbeat DESC)';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'component_heartbeats'
                  AND column_name = 'component'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_component_time ON component_heartbeats(component, last_heartbeat DESC)';
            END IF;
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_heartbeats_stale ON component_heartbeats(last_heartbeat)';
        END IF;
    END IF;

    IF to_regclass('public.system_events') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'system_events'
              AND column_name = 'severity'
        ) AND EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'system_events'
              AND column_name = 'created_at'
        ) THEN
            EXECUTE 'CREATE INDEX IF NOT EXISTS idx_system_events_severity_time ON system_events(severity, created_at DESC)';
        END IF;

        IF EXISTS (
            SELECT 1
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND table_name = 'system_events'
              AND column_name = 'created_at'
        ) THEN
            IF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'system_events'
                  AND column_name = 'component'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_system_events_component_time ON system_events(component, created_at DESC)';
            ELSIF EXISTS (
                SELECT 1
                FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name = 'system_events'
                  AND column_name = 'component_name'
            ) THEN
                EXECUTE 'CREATE INDEX IF NOT EXISTS idx_system_events_component_time ON system_events(component_name, created_at DESC)';
            END IF;
        END IF;
    END IF;
END $$;
