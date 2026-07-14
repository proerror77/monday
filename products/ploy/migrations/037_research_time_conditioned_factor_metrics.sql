-- Migration 037: Persist time-conditioned factor IC research results
--
-- Stores factor correlations segmented by time_remaining_secs so binary-option
-- time-value effects can be queried directly from PostgreSQL instead of being
-- trapped in ad hoc logs.

CREATE TABLE IF NOT EXISTS research_time_conditioned_factor_metrics (
    analysis_scope TEXT NOT NULL,
    start_ts TIMESTAMPTZ NOT NULL,
    end_ts TIMESTAMPTZ NOT NULL,
    symbols_csv TEXT NOT NULL,
    label TEXT NOT NULL,
    factor TEXT NOT NULL,
    bucket_start_secs INTEGER NOT NULL,
    bucket_end_secs INTEGER NOT NULL,
    bin_secs INTEGER NOT NULL,
    min_points INTEGER NOT NULL,
    max_windows INTEGER NOT NULL,
    lob_sample_secs INTEGER NOT NULL,
    n INTEGER NOT NULL,
    pearson_ic DOUBLE PRECISION,
    spearman_ic DOUBLE PRECISION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (analysis_scope, label, factor, bucket_start_secs, bucket_end_secs)
);

CREATE INDEX IF NOT EXISTS idx_research_time_conditioned_factor_metrics_lookup
    ON research_time_conditioned_factor_metrics(
        label,
        factor,
        bucket_start_secs,
        bucket_end_secs
    );
