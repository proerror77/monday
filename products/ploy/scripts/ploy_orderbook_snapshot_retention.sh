#!/usr/bin/env bash
set -euo pipefail

DB_NAME="${PLOY_DB_NAME:-ploy}"
RETENTION_DAYS="${PLOY_ORDERBOOK_SNAPSHOT_RETENTION_DAYS:-2}"
LOOKAHEAD_DAYS="${PLOY_ORDERBOOK_SNAPSHOT_LOOKAHEAD_DAYS:-3}"
BATCH_SIZE="${PLOY_ORDERBOOK_SNAPSHOT_BATCH_SIZE:-25000}"
MAX_BATCHES="${PLOY_ORDERBOOK_SNAPSHOT_MAX_BATCHES:-2}"
ARCHIVE_DIR="${PLOY_CLOB_BOOK_ARCHIVE_DIR:-/opt/ploy/data/lake/orderbook_snapshots}"
REQUIRE_ARCHIVE="${PLOY_CLOB_BOOK_REQUIRE_ARCHIVE:-true}"
DRY_RUN="${PLOY_ORDERBOOK_SNAPSHOT_DRY_RUN:-false}"

is_uint() { [[ "${1:-}" =~ ^[0-9]+$ ]]; }
for value_name in RETENTION_DAYS LOOKAHEAD_DAYS BATCH_SIZE MAX_BATCHES; do
  value="${!value_name}"
  if ! is_uint "$value"; then
    echo "invalid ${value_name}: ${value}" >&2
    exit 2
  fi
done

archived_dates_select() {
  local marker day
  local values=()

  if [[ -d "$ARCHIVE_DIR" ]]; then
    while IFS= read -r marker; do
      day="${marker%/_SUCCESS}"
      day="${day##*/date=}"
      if [[ "$day" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
        values+=("('${day}'::date)")
      fi
    done < <(find "$ARCHIVE_DIR" -mindepth 2 -maxdepth 2 -type f -name _SUCCESS | sort)
  fi

  if (( ${#values[@]} == 0 )); then
    printf "SELECT NULL::date AS archive_day WHERE false"
    return 0
  fi

  local joined
  joined="$(IFS=,; printf '%s' "${values[*]}")"
  printf "SELECT archive_day FROM (VALUES %s) AS archived(archive_day)" "$joined"
}

PSQL=(psql -h 127.0.0.1 -U postgres -d "$DB_NAME" -v ON_ERROR_STOP=1)
export PGPASSWORD="${PGPASSWORD:-postgres}"
ARCHIVED_DATES_SQL="$(archived_dates_select)"

echo "ploy_orderbook_snapshot_retention: retention_days=${RETENTION_DAYS} lookahead_days=${LOOKAHEAD_DAYS} archive_dir=${ARCHIVE_DIR} require_archive=${REQUIRE_ARCHIVE} dry_run=${DRY_RUN}"

if [[ "$DRY_RUN" == "true" ]]; then
  "${PSQL[@]}" <<SQL
CREATE TEMP TABLE archived_clob_book_days AS
${ARCHIVED_DATES_SQL};

SELECT c.relkind, coalesce(pt.partstrat::text, '') AS partstrat
FROM pg_class c
LEFT JOIN pg_partitioned_table pt ON pt.partrelid = c.oid
WHERE c.oid = 'public.clob_orderbook_snapshots'::regclass;

SELECT
  parent.relname AS parent,
  child.relname AS old_partition,
  to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') AS partition_day,
  to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') IN (
    SELECT archive_day FROM archived_clob_book_days
  ) AS archive_complete
FROM pg_inherits
JOIN pg_class child ON child.oid = inhrelid
JOIN pg_class parent ON parent.oid = inhparent
WHERE parent.relname = 'clob_orderbook_snapshots'
  AND substring(child.relname FROM '(20[0-9]{6})$') IS NOT NULL
  AND to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') < current_date - ${RETENTION_DAYS}
ORDER BY child.relname;
SQL
  exit 0
fi

"${PSQL[@]}" <<SQL
SET lock_timeout = '3s';
SET statement_timeout = '5min';
SET client_min_messages = warning;

CREATE TEMP TABLE archived_clob_book_days AS
${ARCHIVED_DATES_SQL};

DO \$\$
DECLARE
  is_partitioned boolean;
  d date;
  end_day date;
  part_name text;
  r record;
  batch_no integer := 0;
  deleted_rows integer := 0;
BEGIN
  SELECT EXISTS (
    SELECT 1
    FROM pg_partitioned_table
    WHERE partrelid = 'public.clob_orderbook_snapshots'::regclass
  ) INTO is_partitioned;

  IF is_partitioned THEN
    d := current_date - ${RETENTION_DAYS};
    end_day := current_date + ${LOOKAHEAD_DAYS};
    WHILE d <= end_day LOOP
      part_name := format('clob_orderbook_snapshots_%s', to_char(d, 'YYYYMMDD'));
      BEGIN
        EXECUTE format(
          'CREATE TABLE IF NOT EXISTS %I PARTITION OF public.clob_orderbook_snapshots FOR VALUES FROM (%L) TO (%L);',
          part_name,
          format('%s 00:00:00+08', d),
          format('%s 00:00:00+08', d + 1)
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
      d := d + 1;
    END LOOP;

    FOR r IN
      SELECT
        child.relname AS child_name,
        to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') AS partition_day
      FROM pg_inherits
      JOIN pg_class child ON child.oid = inhrelid
      JOIN pg_class parent ON parent.oid = inhparent
      WHERE parent.relname = 'clob_orderbook_snapshots'
        AND substring(child.relname FROM '(20[0-9]{6})$') IS NOT NULL
        AND to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') < current_date - ${RETENTION_DAYS}
        AND (
          '${REQUIRE_ARCHIVE}' <> 'true'
          OR to_date(substring(child.relname FROM '(20[0-9]{6})$'), 'YYYYMMDD') IN (
            SELECT archive_day FROM archived_clob_book_days
          )
        )
      ORDER BY child.relname
    LOOP
      RAISE WARNING 'dropping archived orderbook snapshot partition %.% day=%', 'public', r.child_name, r.partition_day;
      EXECUTE format('DROP TABLE IF EXISTS %I.%I', 'public', r.child_name);
    END LOOP;
  ELSE
    LOOP
      WITH doomed AS (
        SELECT id
        FROM clob_orderbook_snapshots
        WHERE received_at < now() - interval '${RETENTION_DAYS} days'
          AND (
            '${REQUIRE_ARCHIVE}' <> 'true'
            OR received_at::date IN (SELECT archive_day FROM archived_clob_book_days)
          )
        ORDER BY received_at
        LIMIT ${BATCH_SIZE}
      )
      DELETE FROM clob_orderbook_snapshots s
      USING doomed
      WHERE s.id = doomed.id;

      GET DIAGNOSTICS deleted_rows = ROW_COUNT;
      batch_no := batch_no + 1;
      RAISE WARNING 'fallback row retention batch %, deleted % rows', batch_no, deleted_rows;
      EXIT WHEN deleted_rows = 0 OR batch_no >= ${MAX_BATCHES};
    END LOOP;
    ANALYZE clob_orderbook_snapshots;
  END IF;
END
\$\$;
SQL

echo "ploy_orderbook_snapshot_retention: done"
