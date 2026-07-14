-- Migration 046: Promote factor registry blockers to a typed queryable column.

ALTER TABLE factor_registry
    ADD COLUMN IF NOT EXISTS blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb;

UPDATE factor_registry
SET blockers_json = metadata->'blockers'
WHERE blockers_json = '[]'::jsonb
  AND metadata ? 'blockers'
  AND jsonb_typeof(metadata->'blockers') = 'array';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'chk_factor_registry_blockers_array'
          AND conrelid = 'factor_registry'::regclass
    ) THEN
        ALTER TABLE factor_registry
            ADD CONSTRAINT chk_factor_registry_blockers_array
            CHECK (jsonb_typeof(blockers_json) = 'array')
            NOT VALID;
    END IF;
END $$;
