-- Ensure clob_trade_ticks has current and near-future daily partitions.
-- Run safely multiple times.

DO $$
DECLARE
    partition_day date;
    start_day date := current_date - 7;
    end_day date := current_date + 14;
    partition_name text;
    range_start text;
    range_end text;
BEGIN
    partition_day := start_day;
    WHILE partition_day <= end_day LOOP
        partition_name := format(
            'clob_trade_ticks_new_%s',
            to_char(partition_day, 'YYYYMMDD')
        );
        range_start := format('%s 00:00:00+08', partition_day);
        range_end := format('%s 00:00:00+08', partition_day + 1);

        BEGIN
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I PARTITION OF clob_trade_ticks FOR VALUES FROM (%L) TO (%L);',
                partition_name,
                range_start,
                range_end
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
END
$$;

SELECT
    child.relname AS partition_name,
    pg_get_expr(child.relpartbound, child.oid) AS partition_expression
FROM pg_inherits
JOIN pg_class parent ON pg_inherits.inhparent = parent.oid
JOIN pg_class child ON pg_inherits.inhrelid = child.oid
WHERE parent.relname = 'clob_trade_ticks'
ORDER BY child.relname;
