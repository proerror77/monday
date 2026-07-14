from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
EXAMPLE = ROOT / "crates" / "ploy-research" / "examples" / "persist_research_trace.rs"
TRACE_PLAN = ROOT / "crates" / "ploy-research" / "examples" / "research_trace_plan.rs"
CARGO = ROOT / "crates" / "ploy-research" / "Cargo.toml"
RUNBOOK = ROOT / "docs" / "runbooks" / "strategy-research-cicd.md"
HOSTED_WALK = ROOT / ".github" / "workflows" / "factor-walk-forward-v2-hosted-artifact.yml"
HOSTED_FACTOR_REVIEW = ROOT / ".github" / "workflows" / "factor-review-v2-hosted-artifact.yml"
AUTOFACTOR_PROMOTION = ROOT / ".github" / "workflows" / "autofactor-strategy-promotion.yml"
TRACE_PLAN_WORKFLOW = ROOT / ".github" / "workflows" / "research-trace-plan.yml"
TANGO_DEPLOY = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"
RESEARCH_SNAPSHOT = ROOT / "crates" / "ploy-research" / "src" / "research_snapshot.rs"
FACTOR_REVIEW_EXAMPLE = ROOT / "crates" / "ploy-research" / "examples" / "factor_review_v2.rs"
FACTOR_WALK_EXAMPLE = ROOT / "crates" / "ploy-research" / "examples" / "factor_walk_forward_v2.rs"
LEGACY_FACTOR_RESEARCH_EXAMPLE = (
    ROOT / "crates" / "ploy-research" / "examples" / "factor_research.rs"
)


class PersistResearchTraceContractTest(unittest.TestCase):
    def test_example_is_registered_with_db_feature(self) -> None:
        cargo = CARGO.read_text(encoding="utf-8")
        self.assertIn('name = "persist_research_trace"', cargo)
        self.assertIn('path = "examples/persist_research_trace.rs"', cargo)
        self.assertIn('name = "research_trace_plan"', cargo)
        self.assertIn('path = "examples/research_trace_plan.rs"', cargo)
        self.assertIn('required-features = ["db"]', cargo)

    def test_writer_persists_all_research_os_tables(self) -> None:
        source = EXAMPLE.read_text(encoding="utf-8")
        for table in [
            "research_dataset_snapshots",
            "factor_registry",
            "factor_evaluations",
            "experiment_trace",
            "candidate_replay_tapes",
            "full_depth_execution_surfaces",
        ]:
            self.assertIn(table, source)
        self.assertIn("ON CONFLICT (data_snapshot_id) DO UPDATE", source)
        self.assertIn("ON CONFLICT (dsl_hash, target, horizon) DO UPDATE", source)
        self.assertIn('const EVIDENCE_STAGE: &str = "factor_attribution"', source)
        self.assertIn('const EVALUATION_KIND: &str = "alpha_search_preview"', source)
        self.assertIn("SELECT eval_id::text", source)
        self.assertIn("trace_hash(", source)
        self.assertIn("--candidate-replay-json", source)
        self.assertIn("--full-depth-execution-surface-json", source)
        self.assertIn("candidate_replay_tape", source)
        self.assertIn("full_depth_execution_surface", source)
        self.assertIn("evaluation_kind = 'candidate_replay'", source)
        self.assertIn("candidate_replay_id = $3", source)
        self.assertIn("ON CONFLICT (candidate_replay_id) DO UPDATE", source)
        self.assertIn("ON CONFLICT (full_depth_execution_surface_id) DO UPDATE", source)
        self.assertIn("full_depth_execution_surface.v1", source)
        self.assertIn("existing_hours + exported_hours", source)
        self.assertIn("full_depth_surface_valid", source)
        self.assertIn("canonical_candidate_replay_evidence_stage", source)
        self.assertIn("contradicts basis", source)
        self.assertIn('"promotion_registry" | "autofactor_promotion" | "strategy_handoff" => "walk_forward"', source)
        self.assertIn("preview_factors(&preview)", source)
        self.assertIn("target_from_preview_path(&path)", source)
        self.assertIn("default_horizon_for_target(&target)", source)
        self.assertIn("autofactor_target_horizon(target)", source)
        self.assertIn("string_field(factor, \"horizon\")", source)

    def test_alpha_search_registry_preview_is_versioned_runtime_contract(self) -> None:
        source = (ROOT / "crates" / "ploy-research" / "src" / "alpha_search.rs").read_text(
            encoding="utf-8"
        )
        required = [
            "struct FactorRegistryPreviewArtifact",
            "autofactor_target_horizon(target)",
            "version: ALPHA_SEARCH_ARTIFACT_VERSION",
            "target: target.to_string()",
            "factors: factor_registry_preview_rows",
            "runtime_contract_for_report",
            "autofactor_runtime_contract_v1",
            '"runtime_score": mapping.runtime_score',
            '"strategy_profile": mapping.strategy_profile',
            '"input_names": ast_input_names',
            '"ast_input_names": ast_input_names',
            '"runtime_input_names": input_projection.runtime_input_names',
            '"input_mappings": input_projection.mappings',
            "runtime_input_projection",
            "runtime_contract_unmapped_factor",
        ]
        for snippet in required:
            self.assertIn(snippet, source)

    def test_persist_research_trace_writes_factor_registry_blockers(self) -> None:
        source = EXAMPLE.read_text(encoding="utf-8")
        self.assertIn("blockers_json", source)
        self.assertIn("blockers_json = EXCLUDED.blockers_json", source)
        self.assertIn("row.blockers.to_string()", source)

    def test_promotion_mapping_is_fail_closed(self) -> None:
        source = EXAMPLE.read_text(encoding="utf-8")
        self.assertIn('promotion_decision: "continue"', source)
        self.assertIn('promotion_status: "candidate"', source)
        self.assertNotIn('promotion_decision: "dry_run_candidate"', source)
        self.assertNotIn('promotion_status: "dry_run"', source)
        self.assertNotIn('promotion_decision: "live_candidate"', source)
        self.assertIn('promotion_decision: "blocked"', source)
        self.assertIn('promotion_status: "blocked"', source)

    def test_runbook_documents_writer_as_trace_not_promotion(self) -> None:
        runbook = RUNBOOK.read_text(encoding="utf-8")
        self.assertIn("persist_research_trace", runbook)
        self.assertIn("does not create dry-run", runbook)
        self.assertIn("or live promotion evidence", runbook)
        self.assertIn("Durable research trace", runbook)

    def test_snapshot_surfaces_use_canonical_gate_categories(self) -> None:
        source = RESEARCH_SNAPSHOT.read_text(encoding="utf-8")
        self.assertIn("gate_category", source)
        for category in [
            "required_for_prediction",
            "required_for_execution",
            "optional_context",
        ]:
            self.assertIn(category, source)

    def test_sampled_snapshot_contract_avoids_full_data_language(self) -> None:
        forbidden = [
            "full research snapshot",
            "full snapshot",
            "full-snapshot",
            "full payload",
            "full research-snapshot",
        ]
        roots = ["docs", ".github", "scripts", "crates", "tests"]
        offenders: list[str] = []
        for root in roots:
            for path in (ROOT / root).rglob("*"):
                if path == Path(__file__):
                    continue
                if not path.is_file() or "target" in path.parts:
                    continue
                if path.suffix not in {".md", ".py", ".rs", ".yml", ".yaml", ".js"}:
                    continue
                text = path.read_text(encoding="utf-8", errors="ignore").lower()
                for needle in forbidden:
                    if needle in text:
                        offenders.append(f"{path.relative_to(ROOT)}: {needle}")
        self.assertEqual([], offenders)

    def test_active_sampled_snapshot_api_uses_canonical_names(self) -> None:
        forbidden_identifiers = [
            "upload_" + "full_snapshot",
            "full_" + "snapshot_embedded",
            "SNAPSHOT_UPLOAD_" + "FULL_SNAPSHOT",
        ]
        active_paths = [
            ROOT / ".github" / "workflows" / "research-snapshot.yml",
            ROOT / ".github" / "workflows" / "factor-review-v2-hosted-artifact.yml",
            ROOT / ".github" / "workflows" / "factor-walk-forward-v2-hosted-artifact.yml",
            ROOT / "scripts" / "research_manager_execute_plan.py",
            ROOT / "scripts" / "run_settlement_probability_prd_gate.py",
        ]
        offenders: list[str] = []
        for path in active_paths:
            text = path.read_text(encoding="utf-8")
            for needle in forbidden_identifiers:
                if needle in text:
                    offenders.append(f"{path.relative_to(ROOT)}: {needle}")
        self.assertEqual([], offenders)

        snapshot_workflow = (ROOT / ".github" / "workflows" / "research-snapshot.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("upload_sampled_snapshot", snapshot_workflow)
        self.assertIn("sampled_snapshot_embedded=", snapshot_workflow)
        self.assertIn('"upload_" + "full_snapshot": "upload_sampled_snapshot"', snapshot_workflow)

    def test_hosted_walk_forward_persists_trace_by_default(self) -> None:
        workflow = HOSTED_WALK.read_text(encoding="utf-8")
        persist_step = workflow.split("- name: Persist Research OS trace", 1)[1].split(
            "- name: Create config PR from ready handoff",
            1,
        )[0]
        required_snippets = [
            '"persist_research_trace":true',
            '"persist_research_trace": "true"',
            "WALK_PERSIST_RESEARCH_TRACE",
            "--example persist_research_trace",
            "Persist Research OS trace",
            "RESEARCH_OS_DATABASE_URL",
            "PLOY_DATABASE_URL",
            "PLOY_RESEARCH_DATABASE_URL",
            "PLOY_DB_URL",
            "Research trace DB host ${db_host} is private",
            "tango-1-1:${remote_dir}/input.tar",
            "/opt/ploy/bin/persist-research-trace",
            'DATABASE_URL="${db_url}" "${persist_bin}"',
            "artifacts/research-trace/persisted.env",
            "artifacts/factor-walk-forward-v2-upload/research-trace/persisted.env",
            "create_config_pr requires successful durable Research OS trace persistence.",
            "chain_next_run requires successful durable Research OS trace persistence.",
            "create_handoff_issue requires successful durable Research OS trace persistence.",
            "--alpha-search-dir artifacts/factor-walk-forward-v2/alpha-search",
            "--registry-json artifacts/factor-walk-forward-v2/autofactor-factor-registry.json",
            "--promotion-json artifacts/factor-walk-forward-v2/autofactor-strategy-promotion.json",
            "--handoff-json artifacts/factor-walk-forward-v2/autofactor-strategy-handoff.json",
            "--candidate-replay-json",
            "--full-depth-execution-surface-json",
            "candidate-strategy-replay/candidate-strategy-replay.json",
            "full-depth-execution-surface/full-depth-execution-surface.json",
            "--snapshot-manifest-json artifacts/research-snapshot/manifest.json",
            "validate_autofactor_handoff_replay_gate.py",
            "factor walk-forward report is required for durable trace persistence.",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, workflow)
        self.assertNotIn("if: always()", persist_step)

    def test_standalone_autofactor_promotion_side_effects_require_trace(self) -> None:
        workflow = AUTOFACTOR_PROMOTION.read_text(encoding="utf-8")
        required_snippets = [
            "SOURCE_TRACE_MARKER_PATH",
            "SOURCE_SNAPSHOT_MANIFEST_PATH",
            "*/research-trace/persisted.env",
            "*/snapshot-provenance/source.txt",
            "*/snapshot-provenance/manifest.json",
            "--snapshot-manifest-json",
            "validate_autofactor_handoff_replay_gate.py",
            "AutoFactor promotion side effects require successful durable Research OS trace persistence.",
            "AutoFactor promotion side effects reject legacy/debug/self-hosted source artifacts.",
            "direct_db_debug=true|canonical_result=no|registry=runner-local|runner=self-hosted|runner=ploy-ci-1",
            "create_handoff_issue requires successful durable Research OS trace persistence.",
            "create_config_pr requires successful durable Research OS trace persistence.",
            "Config branch was pushed, but automatic PR creation failed",
            "Manual PR URL:",
            "The workflow result remains successful because the research artifact and reviewable branch were produced.",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, workflow)

    def test_tango_deploy_installs_persist_trace_binary(self) -> None:
        workflow = TANGO_DEPLOY.read_text(encoding="utf-8")
        deploy_helper = (ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py").read_text(
            encoding="utf-8"
        )
        for source in [workflow, deploy_helper]:
            self.assertIn("persist-research-trace", source)
            self.assertIn("research-trace-plan", source)
        self.assertIn("--example persist_research_trace", workflow)
        self.assertIn("--example research_trace_plan", workflow)
        self.assertIn("044_candidate_replay_tapes.sql", workflow)
        self.assertIn("045_candidate_replay_basis_stage_constraint.sql", workflow)
        self.assertIn("047_full_depth_execution_surfaces.sql", workflow)
        self.assertIn("048_official_settlement_coverage_checks.sql", workflow)

    def test_research_trace_plan_reads_durable_tables(self) -> None:
        source = TRACE_PLAN.read_text(encoding="utf-8")
        for table in [
            "experiment_trace",
            "factor_registry",
            "factor_evaluations",
            "candidate_replay_tapes",
            "research_dataset_snapshots",
            "full_depth_execution_surfaces",
            "official_settlement_coverage_checks",
        ]:
            self.assertIn(table, source)
        self.assertIn("plan_next_research", source)
        self.assertIn('"research_trace_plan.v1"', source)
        self.assertIn("GROUP BY run_id", source)
        self.assertIn("source_surface_blockers", source)
        self.assertIn("latest_full_depth_execution_surfaces", source)
        self.assertIn("latest_official_settlement_coverage_checks", source)
        self.assertIn("ready_strategy_handoffs", source)
        self.assertIn("runtime_ready_factor_candidates", source)
        self.assertIn("ready_candidate_replays", source)
        self.assertIn("runtime_ready_candidates", source)
        self.assertIn("ready_handoffs", source)
        self.assertIn("valid = true", source)
        self.assertIn("execution_surfaces", source)
        self.assertIn("settlement_surfaces", source)
        self.assertIn("missing_blocks_promotion", source)

    def test_trace_plan_workflow_accepts_candidate_runtime_replay_theme(self) -> None:
        workflow = TRACE_PLAN_WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("candidate_to_runtime_replay", workflow)
        self.assertIn("continue_search", workflow)
        self.assertIn("fix_runtime", workflow)
        self.assertIn("blocker_actions = plan.get", workflow)
        self.assertIn("## Blocker Actions", workflow)

    def test_research_trace_plan_workflow_uses_deployed_tango_binary(self) -> None:
        workflow = TRACE_PLAN_WORKFLOW.read_text(encoding="utf-8")
        required = [
            "Research Trace Plan",
            "Build Research Manager plan from durable trace",
            "/opt/ploy/bin/research-trace-plan",
            "RESEARCH_OS_DATABASE_URL",
            "PLOY_DATABASE_URL",
            "PLOY_RESEARCH_DATABASE_URL",
            "PLOY_DB_URL",
            "research-trace-plan.json",
            "research-trace-plan.md",
            "actions/upload-artifact@v4",
            "gh issue comment",
        ]
        for snippet in required:
            self.assertIn(snippet, workflow)
        self.assertNotIn("cargo build", workflow)
        self.assertNotIn("cargo run", workflow)
        self.assertNotIn("StrictHostKeyChecking no", workflow)

    def test_collect_full_depth_workflow_persists_standalone_surface(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "collect-full-depth-execution-surface.yml").read_text(
            encoding="utf-8"
        )
        required = [
            '"persist_research_trace":true',
            '"max_hours":48',
            '"fail_if_incomplete":true',
            "scripts/persist_full_depth_execution_surface.py",
            "full-depth-execution-surface-persist.json",
            "--db-url \"${PLOY_DATABASE__URL}\"",
            "--require-valid",
            "Durable Trace Persistence",
            "source_workflow",
            "workflow_run_id",
            "workflow_run_url",
            "artifact_name",
        ]
        for snippet in required:
            self.assertIn(snippet, workflow)

    def test_factor_router_workflows_are_removed(self) -> None:
        self.assertFalse((ROOT / ".github" / "workflows" / "factor-review-v2.yml").exists())
        self.assertFalse((ROOT / ".github" / "workflows" / "factor-walk-forward-v2.yml").exists())
        for path in [HOSTED_FACTOR_REVIEW, HOSTED_WALK]:
            workflow = path.read_text(encoding="utf-8")
            self.assertIn("snapshot_run_id", workflow)
            self.assertNotIn("allow_direct_db_debug", workflow)
            self.assertNotIn("legacy_db_debug_ack", workflow)
            self.assertNotIn("runs-on: [self-hosted, ploy-ci-1]", workflow)
            self.assertNotIn("--allow-direct-db-debug", workflow)

    def test_direct_db_factor_research_entrypoints_are_removed(self) -> None:
        self.assertFalse((ROOT / "scripts" / "run_factor_research.sh").exists())
        self.assertFalse((ROOT / "scripts" / "run_factor_research_matrix.sh").exists())
        self.assertFalse(LEGACY_FACTOR_RESEARCH_EXAMPLE.exists())
        for path in [FACTOR_REVIEW_EXAMPLE, FACTOR_WALK_EXAMPLE]:
            source = path.read_text(encoding="utf-8")
            self.assertNotIn("--allow-direct-db-debug", source)
            self.assertNotIn("allow_direct_db_debug", source)
            self.assertNotIn("load_from_database_with_options", source)
            self.assertNotIn("load_research_lob_snapshots_sampled", source)
        cargo = CARGO.read_text(encoding="utf-8")
        self.assertNotIn('name = "factor_research"', cargo)
        deploy = TANGO_DEPLOY.read_text(encoding="utf-8")
        deploy_helper = (
            ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py"
        ).read_text(encoding="utf-8")
        self.assertNotIn("--example factor_research", deploy)
        self.assertNotIn("examples/factor_research", deploy)
        self.assertIn('rm -f "${{DEPLOY_ROOT}}/bin/factor-research"', deploy_helper)


if __name__ == "__main__":
    unittest.main()
