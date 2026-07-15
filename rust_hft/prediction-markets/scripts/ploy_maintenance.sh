#!/usr/bin/env bash
set -euo pipefail

# Host + DB maintenance for always-on trading workloads.
#
# Goals:
# - keep the root disk from filling (logs, journals)
# - enforce retention on high-volume tables (ticks/observations)
#
# Intended to be run by systemd as root (see deployment/ploy-maintenance.*).

DB_NAME="${PLOY_DB_NAME:-ploy}"
LOG_DIR="${LOG_DIR:-/opt/ploy/logs}"
DATA_DIR="${PLOY_DATA_DIR:-/opt/ploy/data}"
TMP_DIR="${PLOY_TMP_DIR:-/opt/ploy/tmp}"
EXTRA_LOG_DIRS="${PLOY_EXTRA_LOG_DIRS:-/var/log/ploy}"
CLOB_BOOK_ARCHIVE_DIR="${PLOY_CLOB_BOOK_ARCHIVE_DIR:-$DATA_DIR/lake/orderbook_snapshots}"

RETENTION_CLOB_TICKS_DAYS="${PLOY_RETENTION_CLOB_TICKS_DAYS:-7}"
RETENTION_CLOB_BOOK_DAYS="${PLOY_RETENTION_CLOB_BOOK_DAYS:-7}"
RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS="${PLOY_RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS:-7}"
RETENTION_CLOB_TRADES_DAYS="${PLOY_RETENTION_CLOB_TRADES_DAYS:-7}"
RETENTION_CLOB_ALERTS_DAYS="${PLOY_RETENTION_CLOB_ALERTS_DAYS:-7}"
RETENTION_BINANCE_TICKS_DAYS="${PLOY_RETENTION_BINANCE_TICKS_DAYS:-7}"
RETENTION_BINANCE_AGGTRADE_DAYS="${PLOY_RETENTION_BINANCE_AGGTRADE_DAYS:-7}"
RETENTION_BINANCE_LOB_DAYS="${PLOY_RETENTION_BINANCE_LOB_DAYS:-7}"
RETENTION_DERIBIT_IV_DAYS="${PLOY_RETENTION_DERIBIT_IV_DAYS:-7}"
RETENTION_CEX_PUBLIC_DAYS="${PLOY_RETENTION_CEX_PUBLIC_DAYS:-30}"
RETENTION_NBA_OBS_DAYS="${PLOY_RETENTION_NBA_OBS_DAYS:-7}"
RETENTION_ORDER_EXEC_DAYS="${PLOY_RETENTION_ORDER_EXEC_DAYS:-7}"
RETENTION_LOG_DAYS="${PLOY_RETENTION_LOG_DAYS:-7}"
RETENTION_TMP_DAYS="${PLOY_RETENTION_TMP_DAYS:-2}"
RETENTION_RECORDING_DAYS="${PLOY_RETENTION_RECORDING_DAYS:-7}"
RETENTION_PARQUET_DAYS="${PLOY_RETENTION_PARQUET_DAYS:-14}"
RETENTION_LOG_MAX_FILE_MB="${PLOY_RETENTION_LOG_MAX_FILE_MB:-512}"
RETENTION_LOG_MAX_DIR_MB="${PLOY_RETENTION_LOG_MAX_DIR_MB:-1024}"
RETENTION_TMP_MAX_DIR_MB="${PLOY_RETENTION_TMP_MAX_DIR_MB:-1024}"
RETENTION_RECORDING_MAX_DIR_MB="${PLOY_RETENTION_RECORDING_MAX_DIR_MB:-4096}"
RETENTION_PARQUET_MAX_DIR_MB="${PLOY_RETENTION_PARQUET_MAX_DIR_MB:-8192}"
CLOB_BOOK_DELETE_BATCH_SIZE="${PLOY_CLOB_BOOK_DELETE_BATCH_SIZE:-100000}"
CLOB_BOOK_DELETE_MAX_BATCHES="${PLOY_CLOB_BOOK_DELETE_MAX_BATCHES:-20}"
JOURNAL_VACUUM_SIZE="${PLOY_JOURNAL_VACUUM_SIZE:-200M}"
DERIBIT_PARTITION_LOOKBACK_DAYS="${PLOY_DERIBIT_PARTITION_LOOKBACK_DAYS:-7}"
DERIBIT_PARTITION_LOOKAHEAD_DAYS="${PLOY_DERIBIT_PARTITION_LOOKAHEAD_DAYS:-14}"
REFRESH_RESEARCH_WINDOWS="${PLOY_REFRESH_RESEARCH_WINDOWS:-false}"
LOGS_ONLY="${PLOY_MAINTENANCE_LOGS_ONLY:-false}"
REQUIRE_CLOB_BOOK_ARCHIVE="${PLOY_CLOB_BOOK_REQUIRE_ARCHIVE:-true}"

is_uint() {
  [[ "${1:-}" =~ ^[0-9]+$ ]]
}

if ! is_uint "$RETENTION_CLOB_TICKS_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_TICKS_DAYS: $RETENTION_CLOB_TICKS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_BOOK_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_BOOK_DAYS: $RETENTION_CLOB_BOOK_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS: $RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_TRADES_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_TRADES_DAYS: $RETENTION_CLOB_TRADES_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CLOB_ALERTS_DAYS"; then
  echo "invalid PLOY_RETENTION_CLOB_ALERTS_DAYS: $RETENTION_CLOB_ALERTS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_BINANCE_TICKS_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_TICKS_DAYS: $RETENTION_BINANCE_TICKS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_BINANCE_AGGTRADE_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_AGGTRADE_DAYS: $RETENTION_BINANCE_AGGTRADE_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_BINANCE_LOB_DAYS"; then
  echo "invalid PLOY_RETENTION_BINANCE_LOB_DAYS: $RETENTION_BINANCE_LOB_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_DERIBIT_IV_DAYS"; then
  echo "invalid PLOY_RETENTION_DERIBIT_IV_DAYS: $RETENTION_DERIBIT_IV_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_CEX_PUBLIC_DAYS"; then
  echo "invalid PLOY_RETENTION_CEX_PUBLIC_DAYS: $RETENTION_CEX_PUBLIC_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_NBA_OBS_DAYS"; then
  echo "invalid PLOY_RETENTION_NBA_OBS_DAYS: $RETENTION_NBA_OBS_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_ORDER_EXEC_DAYS"; then
  echo "invalid PLOY_RETENTION_ORDER_EXEC_DAYS: $RETENTION_ORDER_EXEC_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_LOG_DAYS"; then
  echo "invalid PLOY_RETENTION_LOG_DAYS: $RETENTION_LOG_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_TMP_DAYS"; then
  echo "invalid PLOY_RETENTION_TMP_DAYS: $RETENTION_TMP_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_RECORDING_DAYS"; then
  echo "invalid PLOY_RETENTION_RECORDING_DAYS: $RETENTION_RECORDING_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_PARQUET_DAYS"; then
  echo "invalid PLOY_RETENTION_PARQUET_DAYS: $RETENTION_PARQUET_DAYS" >&2
  exit 2
fi
if ! is_uint "$RETENTION_LOG_MAX_FILE_MB"; then
  echo "invalid PLOY_RETENTION_LOG_MAX_FILE_MB: $RETENTION_LOG_MAX_FILE_MB" >&2
  exit 2
fi
if ! is_uint "$RETENTION_LOG_MAX_DIR_MB"; then
  echo "invalid PLOY_RETENTION_LOG_MAX_DIR_MB: $RETENTION_LOG_MAX_DIR_MB" >&2
  exit 2
fi
if ! is_uint "$RETENTION_TMP_MAX_DIR_MB"; then
  echo "invalid PLOY_RETENTION_TMP_MAX_DIR_MB: $RETENTION_TMP_MAX_DIR_MB" >&2
  exit 2
fi
if ! is_uint "$RETENTION_RECORDING_MAX_DIR_MB"; then
  echo "invalid PLOY_RETENTION_RECORDING_MAX_DIR_MB: $RETENTION_RECORDING_MAX_DIR_MB" >&2
  exit 2
fi
if ! is_uint "$RETENTION_PARQUET_MAX_DIR_MB"; then
  echo "invalid PLOY_RETENTION_PARQUET_MAX_DIR_MB: $RETENTION_PARQUET_MAX_DIR_MB" >&2
  exit 2
fi
if ! is_uint "$CLOB_BOOK_DELETE_BATCH_SIZE"; then
  echo "invalid PLOY_CLOB_BOOK_DELETE_BATCH_SIZE: $CLOB_BOOK_DELETE_BATCH_SIZE" >&2
  exit 2
fi
if ! is_uint "$CLOB_BOOK_DELETE_MAX_BATCHES"; then
  echo "invalid PLOY_CLOB_BOOK_DELETE_MAX_BATCHES: $CLOB_BOOK_DELETE_MAX_BATCHES" >&2
  exit 2
fi
if ! is_uint "$DERIBIT_PARTITION_LOOKBACK_DAYS"; then
  echo "invalid PLOY_DERIBIT_PARTITION_LOOKBACK_DAYS: $DERIBIT_PARTITION_LOOKBACK_DAYS" >&2
  exit 2
fi
if ! is_uint "$DERIBIT_PARTITION_LOOKAHEAD_DAYS"; then
  echo "invalid PLOY_DERIBIT_PARTITION_LOOKAHEAD_DAYS: $DERIBIT_PARTITION_LOOKAHEAD_DAYS" >&2
  exit 2
fi

echo "ploy_maintenance: db=${DB_NAME} log_dir=${LOG_DIR} data_dir=${DATA_DIR} tmp_dir=${TMP_DIR} extra_log_dirs=${EXTRA_LOG_DIRS} clob_book_archive_dir=${CLOB_BOOK_ARCHIVE_DIR} clob_book_require_archive=${REQUIRE_CLOB_BOOK_ARCHIVE} clob_ticks_days=${RETENTION_CLOB_TICKS_DAYS} clob_book_days=${RETENTION_CLOB_BOOK_DAYS} clob_obh_days=${RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS} clob_trades_days=${RETENTION_CLOB_TRADES_DAYS} clob_alerts_days=${RETENTION_CLOB_ALERTS_DAYS} binance_ticks_days=${RETENTION_BINANCE_TICKS_DAYS} binance_aggtrade_days=${RETENTION_BINANCE_AGGTRADE_DAYS} binance_lob_days=${RETENTION_BINANCE_LOB_DAYS} deribit_iv_days=${RETENTION_DERIBIT_IV_DAYS} cex_public_days=${RETENTION_CEX_PUBLIC_DAYS} nba_obs_days=${RETENTION_NBA_OBS_DAYS} order_exec_days=${RETENTION_ORDER_EXEC_DAYS} log_days=${RETENTION_LOG_DAYS} tmp_days=${RETENTION_TMP_DAYS} recording_days=${RETENTION_RECORDING_DAYS} parquet_days=${RETENTION_PARQUET_DAYS} log_max_file_mb=${RETENTION_LOG_MAX_FILE_MB} log_max_dir_mb=${RETENTION_LOG_MAX_DIR_MB} tmp_max_dir_mb=${RETENTION_TMP_MAX_DIR_MB} recording_max_dir_mb=${RETENTION_RECORDING_MAX_DIR_MB} parquet_max_dir_mb=${RETENTION_PARQUET_MAX_DIR_MB} clob_book_delete_batch_size=${CLOB_BOOK_DELETE_BATCH_SIZE} clob_book_delete_max_batches=${CLOB_BOOK_DELETE_MAX_BATCHES} deribit_partition_lookback_days=${DERIBIT_PARTITION_LOOKBACK_DAYS} deribit_partition_lookahead_days=${DERIBIT_PARTITION_LOOKAHEAD_DAYS} refresh_research_windows=${REFRESH_RESEARCH_WINDOWS} logs_only=${LOGS_ONLY}"

archived_clob_book_dates_select() {
  local marker day
  local values=()

  if [[ -d "$CLOB_BOOK_ARCHIVE_DIR" ]]; then
    while IFS= read -r marker; do
      day="${marker%/_SUCCESS}"
      day="${day##*/date=}"
      if [[ "$day" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
        values+=("('${day}'::date)")
      fi
    done < <(find "$CLOB_BOOK_ARCHIVE_DIR" -mindepth 2 -maxdepth 2 -type f -name _SUCCESS | sort)
  fi

  if (( ${#values[@]} == 0 )); then
    printf "SELECT NULL::date AS archive_day WHERE false"
    return 0
  fi

  local joined
  joined="$(IFS=,; printf '%s' "${values[*]}")"
  printf "SELECT archive_day FROM (VALUES %s) AS archived(archive_day)" "$joined"
}

prune_log_dir() {
  local dir="$1"
  local current_mb entry path size timestamp

  [[ -d "$dir" ]] || return 0

  echo "Pruning logs in ${dir}"
  find "$dir" -maxdepth 1 -type f -mtime +"$RETENTION_LOG_DAYS" \
    \( -name '*.log' -o -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
    -delete
  find "$dir" -maxdepth 1 -type f -mtime +1 -size +"${RETENTION_LOG_MAX_FILE_MB}"M \
    \( -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log.*' -o -name 'platform.log.*' \) \
    -delete
  find "$dir" -maxdepth 1 -type f -mtime +1 ! -name '*.gz' \
    \( -name '*.log' -o -name '*.log.*' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
    -print0 | xargs -0 -r gzip -9
  find "$dir" -maxdepth 1 -type f -name '*.gz' -mtime +"$RETENTION_LOG_DAYS" -delete

  current_mb=$(du -sm "$dir" | awk '{print $1}')
  while (( current_mb > RETENTION_LOG_MAX_DIR_MB )); do
    entry=$(find "$dir" -maxdepth 1 -type f \
      \( -name '*.log' -o -name '*.log.*' -o -name '*.log.*.gz' -o -name 'ploy.log*' -o -name 'platform.log*' \) \
      -printf '%s %T@ %p\n' | sort -nr | head -1)
    [[ -n "$entry" ]] || break
    size="${entry%% *}"
    entry="${entry#* }"
    timestamp="${entry%% *}"
    path="${entry#* }"
    echo "Deleting ${path} (${size} bytes, ts=${timestamp})"
    rm -f -- "$path"
    current_mb=$(du -sm "$dir" | awk '{print $1}')
  done
  echo "Log dir ${dir} is now ${current_mb}M"
}

prune_tree_dir() {
  local dir="$1"
  local label="$2"
  local days="$3"
  local max_mb="$4"
  local current_mb entry path size timestamp

  [[ -d "$dir" ]] || return 0

  echo "Pruning ${label} in ${dir}: days=${days} max_mb=${max_mb}"
  find "$dir" -mindepth 1 -maxdepth 1 -mtime +"$days" -print -exec rm -rf {} +

  current_mb=$(du -sm "$dir" | awk '{print $1}')
  while (( current_mb > max_mb )); do
    entry=$(find "$dir" -mindepth 1 -maxdepth 1 -exec du -sm {} + | sort -nr | head -1)
    [[ -n "$entry" ]] || break
    size="${entry%%[[:space:]]*}"
    path="${entry#*[[:space:]]}"
    echo "Deleting ${label} artifact ${path} (${size}M)"
    rm -rf -- "$path"
    current_mb=$(du -sm "$dir" | awk '{print $1}')
  done
  echo "${label} dir ${dir} is now ${current_mb}M"
}

if [[ "$LOGS_ONLY" != "true" ]]; then
  if [[ -n "${DATABASE_URL:-}" ]]; then
    PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)
  elif command -v runuser >/dev/null 2>&1; then
    PSQL=(runuser -u postgres -- psql -d "$DB_NAME" -v ON_ERROR_STOP=1)
  else
    # Fallback for minimal distros.
    PSQL=(su -s /bin/bash postgres -c "psql -d \"$DB_NAME\" -v ON_ERROR_STOP=1")
  fi

  ARCHIVED_CLOB_BOOK_DATES_SQL="$(archived_clob_book_dates_select)"

  echo "==> DB retention"
  if [[ "$REFRESH_RESEARCH_WINDOWS" == "true" ]]; then
    "${PSQL[@]}" <<SQL
-- Refresh research valid windows materialized view (if it exists).
-- CONCURRENTLY avoids locking reads during refresh; requires the UNIQUE index.
SELECT 'REFRESH MATERIALIZED VIEW CONCURRENTLY research_valid_windows;'
WHERE to_regclass('public.research_valid_windows') IS NOT NULL \\gexec
SQL
  else
    echo "Skipping research_valid_windows refresh; set PLOY_REFRESH_RESEARCH_WINDOWS=true to enable."
  fi

  "${PSQL[@]}" <<SQL
CREATE TEMP TABLE archived_clob_book_days AS
${ARCHIVED_CLOB_BOOK_DATES_SQL};

DO \$\$
DECLARE
  partition_day date := current_date - ${DERIBIT_PARTITION_LOOKBACK_DAYS};
  end_day date := current_date + ${DERIBIT_PARTITION_LOOKAHEAD_DAYS};
  partition_name text;
BEGIN
  IF to_regclass('public.deribit_iv_ticks') IS NOT NULL THEN
    WHILE partition_day <= end_day LOOP
      partition_name := format('deribit_iv_ticks_new_%s', to_char(partition_day, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF deribit_iv_ticks FOR VALUES FROM (%L) TO (%L);',
          partition_name,
          format('%s 00:00:00+08', partition_day),
          format('%s 00:00:00+08', partition_day + 1)
        );
      EXCEPTION
        WHEN duplicate_table THEN
          NULL;
        WHEN OTHERS THEN
          IF position('would overlap partition' in SQLERRM) > 0 THEN
            NULL;
          ELSE
            RAISE;
          END IF;
      END;
      partition_day := partition_day + 1;
    END LOOP;
  END IF;
END
\$\$;

DO \$\$
DECLARE
  partition_day date := current_date - ${DERIBIT_PARTITION_LOOKBACK_DAYS};
  end_day date := current_date + ${DERIBIT_PARTITION_LOOKAHEAD_DAYS};
  partition_name text;
BEGIN
  IF to_regclass('public.binance_lob_ticks') IS NOT NULL THEN
    WHILE partition_day <= end_day LOOP
      partition_name := format('binance_lob_ticks_new_%s', to_char(partition_day, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF binance_lob_ticks FOR VALUES FROM (%L) TO (%L);',
          partition_name,
          format('%s 00:00:00+08', partition_day),
          format('%s 00:00:00+08', partition_day + 1)
        );
      EXCEPTION
        WHEN duplicate_table THEN
          NULL;
        WHEN OTHERS THEN
          IF position('would overlap partition' in SQLERRM) > 0 THEN
            NULL;
          ELSE
            RAISE;
          END IF;
      END;
      partition_day := partition_day + 1;
    END LOOP;
  END IF;
END
\$\$;

DO \$\$
DECLARE
  partition_day date := current_date - ${DERIBIT_PARTITION_LOOKBACK_DAYS};
  end_day date := current_date + ${DERIBIT_PARTITION_LOOKAHEAD_DAYS};
  partition_name text;
BEGIN
  IF to_regclass('public.clob_trade_ticks') IS NOT NULL THEN
    WHILE partition_day <= end_day LOOP
      partition_name := format('clob_trade_ticks_new_%s', to_char(partition_day, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF clob_trade_ticks FOR VALUES FROM (%L) TO (%L);',
          partition_name,
          format('%s 00:00:00+08', partition_day),
          format('%s 00:00:00+08', partition_day + 1)
        );
      EXCEPTION
        WHEN duplicate_table THEN
          NULL;
        WHEN OTHERS THEN
          IF position('would overlap partition' in SQLERRM) > 0 THEN
            NULL;
          ELSE
            RAISE;
          END IF;
      END;
      partition_day := partition_day + 1;
    END LOOP;
  END IF;
END
\$\$;

DO \$\$
DECLARE
  r record;
BEGIN
  FOR r IN
    SELECT parent.relname AS parent_name, child.relname AS child_name
    FROM pg_inherits
    JOIN pg_class child ON child.oid = inhrelid
    JOIN pg_class parent ON parent.oid = inhparent
    WHERE (
        parent.relname = 'binance_lob_ticks'
        AND to_date(substring(child.relname from '(20[0-9]{6})$'), 'YYYYMMDD')
          < current_date - ${RETENTION_BINANCE_LOB_DAYS}
      ) OR (
        parent.relname = 'clob_trade_ticks'
        AND to_date(substring(child.relname from '(20[0-9]{6})$'), 'YYYYMMDD')
          < current_date - ${RETENTION_CLOB_TRADES_DAYS}
      ) OR (
        parent.relname = 'deribit_iv_ticks'
        AND to_date(substring(child.relname from '(20[0-9]{6})$'), 'YYYYMMDD')
          < current_date - ${RETENTION_DERIBIT_IV_DAYS}
      ) OR (
        parent.relname = 'clob_orderbook_snapshots'
        AND to_date(substring(child.relname from '(20[0-9]{6})$'), 'YYYYMMDD')
          < current_date - ${RETENTION_CLOB_BOOK_DAYS}
        AND (
          '${REQUIRE_CLOB_BOOK_ARCHIVE}' <> 'true'
          OR to_date(substring(child.relname from '(20[0-9]{6})$'), 'YYYYMMDD') IN (
            SELECT archive_day FROM archived_clob_book_days
          )
        )
      )
    ORDER BY parent.relname, child.relname
  LOOP
    RAISE NOTICE 'dropping old partition %.%', 'public', r.child_name;
    EXECUTE format('DROP TABLE IF EXISTS %I.%I', 'public', r.child_name);
  END LOOP;
END
\$\$;

-- TimescaleDB hypertables: drop old chunks so disk can be released without
-- long row-wise deletes.
SELECT format(
  'SELECT drop_chunks(%L::regclass, older_than => INTERVAL ''%s days'');',
  'public.clob_quote_ticks',
  ${RETENTION_CLOB_TICKS_DAYS}
) WHERE to_regclass('public.clob_quote_ticks') IS NOT NULL \\gexec

SELECT format(
  'SELECT drop_chunks(%L::regclass, older_than => INTERVAL ''%s days'');',
  'public.binance_price_ticks',
  ${RETENTION_BINANCE_TICKS_DAYS}
) WHERE to_regclass('public.binance_price_ticks') IS NOT NULL \\gexec

SELECT format(
  'SELECT drop_chunks(%L::regclass, older_than => INTERVAL ''%s days'');',
  'public.binance_agg_trade_ticks',
  ${RETENTION_BINANCE_AGGTRADE_DAYS}
) WHERE to_regclass('public.binance_agg_trade_ticks') IS NOT NULL \\gexec

-- Keep snapshot cleanup bounded and archive-gated. On non-partitioned hosts,
-- this deletes only dates that already have a completed Parquet day marker.
DO \$\$
DECLARE
  batch_no integer := 0;
  deleted_rows integer := 0;
BEGIN
  IF to_regclass('public.clob_orderbook_snapshots') IS NOT NULL THEN
    LOOP
      WITH doomed AS (
        SELECT id
        FROM clob_orderbook_snapshots
        WHERE received_at < NOW() - INTERVAL '${RETENTION_CLOB_BOOK_DAYS} days'
          AND (
            '${REQUIRE_CLOB_BOOK_ARCHIVE}' <> 'true'
            OR received_at::date IN (SELECT archive_day FROM archived_clob_book_days)
          )
        ORDER BY received_at
        LIMIT ${CLOB_BOOK_DELETE_BATCH_SIZE}
      )
      DELETE FROM clob_orderbook_snapshots s
      USING doomed
      WHERE s.id = doomed.id;

      GET DIAGNOSTICS deleted_rows = ROW_COUNT;
      batch_no := batch_no + 1;
      RAISE NOTICE 'clob_orderbook_snapshots retention batch %, deleted % rows', batch_no, deleted_rows;
      EXIT WHEN deleted_rows = 0 OR batch_no >= ${CLOB_BOOK_DELETE_MAX_BATCHES};
    END LOOP;
  END IF;
END
\$\$;

-- Sampled public CEX rows are intentionally bounded independently of raw
-- Polymarket archives.
DO \$\$
DECLARE
  batch_no integer := 0;
  deleted_rows integer := 0;
BEGIN
  IF to_regclass('public.cex_public_market_ticks') IS NOT NULL THEN
    LOOP
      WITH doomed AS (
        SELECT id
        FROM cex_public_market_ticks
        WHERE event_time < NOW() - INTERVAL '${RETENTION_CEX_PUBLIC_DAYS} days'
        ORDER BY event_time
        LIMIT ${CLOB_BOOK_DELETE_BATCH_SIZE}
      )
      DELETE FROM cex_public_market_ticks t
      USING doomed
      WHERE t.id = doomed.id;
      GET DIAGNOSTICS deleted_rows = ROW_COUNT;
      batch_no := batch_no + 1;
      EXIT WHEN deleted_rows = 0 OR batch_no >= ${CLOB_BOOK_DELETE_MAX_BATCHES};
    END LOOP;
  END IF;
END
\$\$;

-- CLOB orderbook-history L2 ticks (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_orderbook_history_ticks WHERE book_ts < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS}
) WHERE to_regclass('public.clob_orderbook_history_ticks') IS NOT NULL \\gexec

-- Trade alerts (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM clob_trade_alerts WHERE created_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_CLOB_ALERTS_DAYS}
) WHERE to_regclass('public.clob_trade_alerts') IS NOT NULL \\gexec

-- Sports observations (moderate volume)
DELETE FROM nba_live_observations
WHERE recorded_at < NOW() - INTERVAL '${RETENTION_NBA_OBS_DAYS} days';

-- Agent execution records (optional; table may not exist on all hosts)
SELECT format(
  'DELETE FROM agent_order_executions WHERE executed_at < NOW() - INTERVAL ''%s days'';',
  ${RETENTION_ORDER_EXEC_DAYS}
) WHERE to_regclass('public.agent_order_executions') IS NOT NULL \\gexec

ANALYZE clob_quote_ticks;
SELECT 'ANALYZE clob_orderbook_snapshots;' WHERE to_regclass('public.clob_orderbook_snapshots') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) clob_orderbook_history_ticks;' WHERE to_regclass('public.clob_orderbook_history_ticks') IS NOT NULL \\gexec
SELECT 'ANALYZE clob_trade_ticks;' WHERE to_regclass('public.clob_trade_ticks') IS NOT NULL \\gexec
SELECT 'VACUUM (ANALYZE) clob_trade_alerts;' WHERE to_regclass('public.clob_trade_alerts') IS NOT NULL \\gexec
SELECT 'ANALYZE binance_price_ticks;' WHERE to_regclass('public.binance_price_ticks') IS NOT NULL \\gexec
SELECT 'ANALYZE binance_agg_trade_ticks;' WHERE to_regclass('public.binance_agg_trade_ticks') IS NOT NULL \\gexec
SELECT 'ANALYZE binance_lob_ticks;' WHERE to_regclass('public.binance_lob_ticks') IS NOT NULL \\gexec
SELECT 'ANALYZE cex_public_market_ticks;' WHERE to_regclass('public.cex_public_market_ticks') IS NOT NULL \\gexec
VACUUM (ANALYZE) nba_live_observations;
SELECT 'VACUUM (ANALYZE) agent_order_executions;' WHERE to_regclass('public.agent_order_executions') IS NOT NULL \\gexec
SQL
else
  echo "Skipping DB retention because PLOY_MAINTENANCE_LOGS_ONLY=true"
fi

echo "==> Log retention"
seen_log_dirs=" "
for dir in "$LOG_DIR" $EXTRA_LOG_DIRS; do
  [[ -n "${dir:-}" ]] || continue
  case "$seen_log_dirs" in
    *" $dir "*) continue ;;
  esac
  seen_log_dirs="${seen_log_dirs}${dir} "
  prune_log_dir "$dir"
done

echo "==> Data artifact retention"
prune_tree_dir "$TMP_DIR" "tmp" "$RETENTION_TMP_DAYS" "$RETENTION_TMP_MAX_DIR_MB"
prune_tree_dir "$DATA_DIR/recordings" "recordings" "$RETENTION_RECORDING_DAYS" "$RETENTION_RECORDING_MAX_DIR_MB"
prune_tree_dir "$DATA_DIR/parquet" "parquet" "$RETENTION_PARQUET_DAYS" "$RETENTION_PARQUET_MAX_DIR_MB"

echo "==> Journald vacuum"
if command -v journalctl >/dev/null 2>&1; then
  journalctl --vacuum-size="$JOURNAL_VACUUM_SIZE" >/dev/null 2>&1 || true
fi

echo "ploy_maintenance: done"
