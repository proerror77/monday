from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
MIGRATION_042 = ROOT / "migrations" / "042_factor_research_os_registry.sql"
MIGRATION_043 = ROOT / "migrations" / "043_research_os_trace_constraints.sql"
MIGRATION_044 = ROOT / "migrations" / "044_candidate_replay_tapes.sql"
MIGRATION_046 = ROOT / "migrations" / "046_factor_registry_blockers.sql"
MIGRATION_047 = ROOT / "migrations" / "047_full_depth_execution_surfaces.sql"
MIGRATION_048 = ROOT / "migrations" / "048_official_settlement_coverage_checks.sql"


def research_os_sql() -> str:
    return "\n".join(
        [
            MIGRATION_042.read_text(encoding="utf-8"),
            MIGRATION_043.read_text(encoding="utf-8"),
            MIGRATION_044.read_text(encoding="utf-8"),
            MIGRATION_046.read_text(encoding="utf-8"),
            MIGRATION_047.read_text(encoding="utf-8"),
            MIGRATION_048.read_text(encoding="utf-8"),
        ]
    )


class FactorResearchOsRegistryMigrationTest(unittest.TestCase):
    def test_factor_registry_tables_and_statuses_exist(self) -> None:
        sql = research_os_sql()
        for table in [
            "factor_registry",
            "research_dataset_snapshots",
            "factor_evaluations",
            "experiment_trace",
            "candidate_replay_tapes",
            "full_depth_execution_surfaces",
            "official_settlement_coverage_checks",
        ]:
            self.assertIn(f"CREATE TABLE IF NOT EXISTS {table}", sql)
        self.assertRegex(
            sql,
            r"status TEXT NOT NULL CHECK .*draft.*compiled.*evaluated.*candidate.*dry_run.*approved.*production.*deprecated",
        )
        self.assertIn("dsl_hash TEXT NOT NULL", sql)
        self.assertNotIn("idx_factor_registry_dsl_hash", MIGRATION_042.read_text(encoding="utf-8"))
        self.assertIn("tmp_factor_registry_dedup", sql)
        self.assertIn("UPDATE factor_evaluations", sql)
        self.assertIn("idx_factor_registry_dsl_target_horizon", sql)
        self.assertIn("ON factor_registry(dsl_hash, target, horizon)", sql)
        self.assertIn("ast_json JSONB NOT NULL", sql)
        self.assertIn("runtime_contract JSONB NOT NULL DEFAULT '{}'::jsonb", sql)
        self.assertIn("chk_factor_registry_blockers_array", sql)
        self.assertIn("source_surfaces_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("input_artifacts_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("dataset_start_ts TIMESTAMPTZ", sql)
        self.assertIn("dataset_end_ts TIMESTAMPTZ", sql)
        self.assertIn("evidence_stage TEXT NOT NULL DEFAULT 'factor_attribution'", sql)
        self.assertIn("evaluation_kind TEXT NOT NULL DEFAULT 'alpha_search_preview'", sql)
        self.assertIn("candidate_replay_id TEXT", sql)
        self.assertIn("promotion_decision TEXT NOT NULL DEFAULT 'not_evaluated'", sql)
        self.assertIn("promotion_status TEXT NOT NULL DEFAULT 'blocked'", sql)
        self.assertIn("blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb", sql)
        self.assertIn("artifact_kind TEXT NOT NULL DEFAULT 'artifact'", sql)
        self.assertIn("candidate_replay_id TEXT", sql)
        self.assertIn("hash_prev TEXT", sql)
        self.assertIn("hash_current TEXT NOT NULL", sql)

    def test_candidate_replay_tapes_schema_and_links_exist(self) -> None:
        sql = research_os_sql()
        required_snippets = [
            "candidate_replay_id TEXT PRIMARY KEY",
            "artifact_sha256 TEXT NOT NULL",
            "CONSTRAINT uq_candidate_replay_tapes_artifact_sha256 UNIQUE (artifact_sha256)",
            "basis TEXT NOT NULL",
            "runtime_score TEXT NOT NULL",
            "strategy_profile TEXT NOT NULL",
            "decision_contract_json JSONB NOT NULL DEFAULT '{}'::jsonb",
            "acceptance_criteria_json JSONB NOT NULL DEFAULT '{}'::jsonb",
            "metrics_json JSONB NOT NULL DEFAULT '{}'::jsonb",
            "blocking_risk_flags_json JSONB NOT NULL DEFAULT '[]'::jsonb",
            "chk_candidate_replay_tapes_basis",
            "runtime_market_update_replay",
            "factor_walk_forward_top_bucket_aggregate",
            "fk_factor_evaluations_candidate_replay",
            "REFERENCES candidate_replay_tapes(candidate_replay_id)",
            "fk_experiment_trace_candidate_replay",
            "idx_candidate_replay_tapes_runtime_score",
            "idx_candidate_replay_tapes_promotion_ready",
            "idx_experiment_trace_candidate_replay",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, sql)

    def test_full_depth_execution_surface_schema_and_links_exist(self) -> None:
        sql = research_os_sql()
        required_snippets = [
            "full_depth_execution_surface_id TEXT PRIMARY KEY",
            "schema_version TEXT NOT NULL DEFAULT 'full_depth_execution_surface.v1'",
            "artifact_sha256 TEXT NOT NULL",
            "CONSTRAINT uq_full_depth_execution_surfaces_artifact_sha256 UNIQUE (artifact_sha256)",
            "surface TEXT NOT NULL",
            "source TEXT NOT NULL",
            "window_start_ts TIMESTAMPTZ NOT NULL",
            "window_end_ts TIMESTAMPTZ NOT NULL",
            "checked_hours INTEGER NOT NULL",
            "existing_hours INTEGER NOT NULL",
            "row_count BIGINT NOT NULL",
            "full_fidelity BOOLEAN NOT NULL DEFAULT false",
            "incomplete BOOLEAN NOT NULL DEFAULT true",
            "valid BOOLEAN NOT NULL DEFAULT false",
            "blockers_json JSONB NOT NULL DEFAULT '[]'::jsonb",
            "idx_full_depth_execution_surfaces_surface_window",
            "idx_full_depth_execution_surfaces_valid",
            "ADD COLUMN IF NOT EXISTS full_depth_execution_surface_id TEXT",
            "fk_experiment_trace_full_depth_execution_surface",
            "REFERENCES full_depth_execution_surfaces(full_depth_execution_surface_id)",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, sql)

    def test_official_settlement_coverage_schema_exists(self) -> None:
        sql = research_os_sql()
        required_snippets = [
            "settlement_coverage_id TEXT PRIMARY KEY",
            "schema_version TEXT NOT NULL DEFAULT 'official_settlement_repair.v1'",
            "surface TEXT NOT NULL DEFAULT 'pm_token_settlements'",
            "symbols_json JSONB NOT NULL DEFAULT '[]'::jsonb",
            "candidate_market_count INTEGER NOT NULL",
            "settlement_token_count INTEGER NOT NULL",
            "unchanged_count INTEGER NOT NULL DEFAULT 0",
            "valid BOOLEAN NOT NULL DEFAULT false",
            "CONSTRAINT uq_official_settlement_coverage_artifact_sha256 UNIQUE (artifact_sha256)",
            "idx_official_settlement_coverage_surface_window",
            "idx_official_settlement_coverage_valid",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, sql)

    def test_experiment_trace_is_append_only_by_trigger(self) -> None:
        sql = research_os_sql()
        self.assertIn("prevent_experiment_trace_update", sql)
        self.assertIn("prevent_experiment_trace_delete", sql)
        self.assertIn("RAISE EXCEPTION 'experiment_trace is append-only'", sql)

    def test_evaluation_trace_constraints_exist(self) -> None:
        sql = research_os_sql()
        required_snippets = [
            "fk_factor_evaluations_data_snapshot",
            "REFERENCES research_dataset_snapshots(data_snapshot_id)",
            "chk_factor_evaluations_dataset_window",
            "dataset_start_ts IS NOT NULL",
            "dataset_end_ts IS NOT NULL",
            "dataset_start_ts < dataset_end_ts",
            "chk_factor_evaluations_evidence_stage",
            "chk_factor_evaluations_evaluation_kind",
            "chk_factor_evaluations_promotion_decision",
            "chk_factor_evaluations_promotion_status",
            "chk_factor_evaluations_blockers_array",
            "jsonb_typeof(blockers_json) = 'array'",
            "chk_experiment_trace_evidence_stage",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, sql)

    def test_tango_deploy_includes_research_os_migrations(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("042_factor_research_os_registry.sql", workflow)
        self.assertIn("043_research_os_trace_constraints.sql", workflow)
        self.assertIn("044_candidate_replay_tapes.sql", workflow)
        self.assertIn("046_factor_registry_blockers.sql", workflow)
        self.assertIn("047_full_depth_execution_surfaces.sql", workflow)
        self.assertIn("048_official_settlement_coverage_checks.sql", workflow)


if __name__ == "__main__":
    unittest.main()
