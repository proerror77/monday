# Horizon-Safe Event Research Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task. REQUIRED DOMAIN SKILL: use event-ml-automl-workflow for every research-runner task.

Goal: Make PM5D and PM15D research impossible to mix implicitly, add the missing portable event-root producer, and enforce the canonical coverage-to-walk-forward evidence sequence before any dry-run handoff.

Architecture: Put one typed horizon contract in shared operator contracts, require it at dataset construction, propagate it through every research/model/handoff artifact, and reject any workflow/runtime mismatch before training or intent generation.

Tech Stack: Rust 1.91, Serde/Schemars, Polars/Parquet, existing `ploy-research` Event ML examples, Python standard library, and GitHub Actions.

## Global Constraints

- Evidence stage for implementation is `implementation hardening`; fixture runs are `diagnostic`, not alpha or promotion evidence.
- Read and obey `docs/PROJECT_SEMANTICS.md` and `docs/runbooks/event-ml-automl-workflow.md` before every task.
- Keep one event as one decision/trade lifecycle unless a future strategy explicitly declares multi-entry accounting.
- Never train with overlapping event IDs, future rows, settlement leakage, or validation/test-derived factor direction.
- Keep model family and hyperparameter selection off the test split.
- Require executable entry price, fees/slippage/latency, payout, PnL, ROI, average entry, maximum drawdown, bankroll framing, and rejected hypotheses.
- PM5D is 300 seconds; PM15D is 900 seconds; PM1H/3600 remains unsupported and fail-closed.
- Do not create a PM15D runtime config/model by copying PM5D values. A PM15D runtime artifact appears only after independent 900-second evidence and handoff.
- Do not run local PostgreSQL or dispatch a workflow.
- Do not add DL or RL implementation. Keep their foundation gates blocked.
- Each task is one atomic commit and stages only its owned paths.

## Foundation Evidence

Before this plan was written, the existing foundation generator ran locally:

```bash
rtk cargo run -p ploy-research --example event_ml_architecture -- \
  --output-dir /tmp/ploy-local-readiness-event-ml-architecture
```

It produced `event-ml-architecture.v1` with status `ready`. This plan preserves its phase order and stop rules; it does not treat that architecture artifact as strategy evidence.

---

### Task 1: Add a shared horizon contract and dataset manifest V2

Files:

- Add `crates/ploy-operator-contracts/src/research.rs`.
- Modify `crates/ploy-operator-contracts/src/lib.rs`.
- Modify `crates/ploy-operator-contracts/src/schemas.rs` and regenerate its new schema.
- Add generated `contracts/schemas/event-horizon-contract.schema.json`.
- Modify `crates/ploy-research/src/dataset/contracts.rs`.
- Modify `crates/ploy-research/src/dataset/chronology.rs`.
- Modify `crates/ploy-research/src/dataset/builder.rs`.
- Modify `crates/ploy-research/src/dataset/export.rs`.
- Modify `crates/ploy-research/src/dataset/mod.rs`.
- Modify `crates/ploy-research/src/factors.rs`.
- Modify all Rust `FactorObservation` and `EventMetadataChronologyInput` struct literals found with `rg`.

Shared contract:

```rust
pub const SUPPORTED_EVENT_MARKET_WINDOWS_SECS: [u32; 2] = [300, 900];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventAccountingLane {
    SettlementProbability,
    Repricing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventSettlementSource {
    OfficialPolymarket,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventHorizonContract {
    pub market_window_secs: u32,
    pub prediction_horizon_secs: u32,
    pub entry_offset_secs: u32,
    pub target_label: String,
    pub accounting_lane: EventAccountingLane,
    pub settlement_source: EventSettlementSource,
    pub allowed_symbols: Vec<String>,
}

impl EventHorizonContract {
    pub fn validate(&self) -> Result<(), String>;
}
```

Field semantics:

- `entry_offset_secs` is seconds remaining until event settlement, matching the existing Event ML `--entry-secs` flag.
- Every contract requires `0 < prediction_horizon_secs <= entry_offset_secs <= market_window_secs`; zero-second labels and horizons extending past settlement are invalid.
- Settlement lane requires target `settlement_up`, source `official_polymarket`, and `prediction_horizon_secs == entry_offset_secs`.
- Repricing lane permits horizons `5`, `10`, `30`, or `60`, requires target `future_up_ask_change_<N>s`, source `not_applicable`, and `prediction_horizon_secs <= entry_offset_secs`.
- Symbols are trimmed uppercase, non-empty, and unique.
- Window 3600 returns the exact error `unsupported market_window_secs 3600; PM1H requires a separate governed profile`.

Dataset V2:

```rust
pub const DATASET_MANIFEST_VERSION: u32 = 2;

pub struct DatasetBuildManifest {
    // existing fields
    pub horizon: EventHorizonContract,
}

pub struct EventChronologyKey {
    // existing fields
    pub market_window_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventMetadataChronologyInput {
    // existing fields
    pub market_window_secs: u32,
}

pub struct EventIndexEntry {
    // existing fields
    pub market_window_secs: u32,
}
```

Make the horizon explicit; provide no implicit 300-second default:

```rust
impl<'a> EventRootDatasetBuildRequest<'a> {
    pub fn new(
        observations: &'a [FactorObservation],
        chronology_events: Vec<EventMetadataChronologyInput>,
        source_window: DatasetSourceWindow,
        horizon: EventHorizonContract,
        artifacts: DatasetArtifacts,
        built_at: DateTime<Utc>,
    ) -> Self;
}
```

Builder rejects before splitting:

- duplicate event ID;
- row window different from manifest horizon;
- `end_time - start_time` different from row window;
- event outside source window;
- event symbol outside allowed symbols;
- source-window symbol outside allowed symbols;
- observation event/symbol different from chronology;
- observation `tick_ts` outside `[start_time, end_time)`;
- observation `time_remaining_secs` different from `(end_time - tick_ts).num_seconds()`;
- repricing target timestamp `tick_ts + prediction_horizon_secs` after the event end, or a selected future row from another event/symbol or after event end;
- any mixed 300/900 set.

Extend repricing observations and Parquet export:

```rust
pub future_up_ask_change_5s: Option<f64>,
pub future_up_ask_change_10s: Option<f64>,
pub future_up_ask_change_30s: Option<f64>,
pub future_up_ask_change_60s: Option<f64>,
```

`DatasetLabelContract` is derived from the horizon target. It no longer implicitly declares both settlement and 30-second repricing as one training target.

Step 1: Add failing tests.

```rust
fn rejects_pm1h_until_governed()
fn settlement_horizon_matches_seconds_to_settlement()
fn repricing_horizon_matches_target_label()
fn builder_rejects_mixed_market_windows()
fn builder_rejects_event_outside_source_window()
fn builder_rejects_symbol_outside_horizon_contract()
fn builder_rejects_observation_symbol_mismatch()
fn builder_rejects_tick_outside_event_or_incorrect_time_remaining()
fn repricing_label_never_reads_past_event_end()
fn horizon_requires_positive_prediction_not_after_entry_or_window()
fn event_index_frame_contains_market_window_secs()
fn future_repricing_labels_cover_5_10_30_60_secs()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-operator-contracts research::tests::rejects_pm1h_until_governed -- --exact
rtk cargo test -p ploy-research --lib dataset::builder::tests::builder_rejects_mixed_market_windows -- --exact
rtk cargo test -p ploy-research --features polars-export --lib dataset::export::tests::event_index_frame_contains_market_window_secs -- --exact
```

Expected RED result: shared types/fields do not exist and the builder accepts implicit/mixed horizons.

Step 3: Implement the contract and update constructors.

- Keep `EventHorizonContract` in operator contracts so research and runtime consume one type.
- Bump the manifest to V2 with no silent upgrade of V1 artifacts.
- Include `market_window_secs` in `event_index.parquet`.
- Preserve the canonical 70/15/15 event-held-out split policy.

Step 4: Regenerate schemas and verify.

```bash
cargo run -p ploy-operator-contracts --example export_schemas
rtk cargo test -p ploy-operator-contracts
rtk cargo test -p ploy-research dataset --lib
rtk cargo test -p ploy-research --features polars-export dataset --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-operator-contracts crates/ploy-research \
  contracts/schemas/event-horizon-contract.schema.json
git commit -m "feat(research): add horizon-safe event dataset contract"
```

---

### Task 2: Add the portable event-root input loader and producer

Files:

- Add `crates/ploy-research/src/dataset/portable.rs`.
- Add `crates/ploy-research/examples/event_root_dataset_producer.rs`.
- Modify `crates/ploy-research/src/research_snapshot.rs`.
- Modify `crates/ploy-research/examples/research_snapshot_compile.rs`.
- Modify `crates/ploy-research/src/dataset/mod.rs`.
- Modify `crates/ploy-research/src/lib.rs`.
- Modify `crates/ploy-research/Cargo.toml` to register the example.
- Modify `crates/ploy-research/examples/event_dataset_rolling_windows.rs`.
- Modify `.github/workflows/research-snapshot.yml` to emit the portable source artifact from the research PostgreSQL snapshot path.
- Modify `tests/workflow_security.rs` with the database-to-portable producer boundary.

Portable contract:

```rust
pub const PORTABLE_EVENT_ROOT_INPUT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableEventRootInputManifest {
    pub manifest_version: u32,
    pub source_window: DatasetSourceWindow,
    pub horizon: EventHorizonContract,
    pub feature_families: Vec<String>,
    pub factor_observations_path: String,
    pub event_chronology_path: String,
}

pub struct PortableEventRootInput {
    pub manifest: PortableEventRootInputManifest,
    pub observations: Vec<FactorObservation>,
    pub chronology_events: Vec<EventMetadataChronologyInput>,
}

pub fn load_portable_event_root_input(
    input_root: &Path,
) -> Result<PortableEventRootInput, PortableInputError>;

#[cfg(feature = "db")]
pub async fn export_portable_event_root_input_from_database(
    pool: &PgPool,
    snapshot: &ResearchSnapshot,
    horizon: &EventHorizonContract,
    output_root: &Path,
) -> anyhow::Result<PortableEventRootInputManifest>;

#[cfg(feature = "polars-export")]
pub fn produce_event_root_dataset(
    input_root: &Path,
    output_root: &Path,
    built_at: DateTime<Utc>,
) -> Result<DatasetBuildManifest, PortableInputError>;
```

Input layout is fixed:

```text
input_manifest.json
factor_observations.jsonl
event_chronology.jsonl
```

Loader rules:

- Read only `input_manifest.json` as the entrypoint.
- Reject absolute paths, prefix/root components, and every `..` component.
- Parse JSONL line-by-line; errors include filename and 1-based line number.
- Portable chronology requires both start/end timestamps; unlike legacy builder input, missing timing is an error, not a skipped row.
- Reject unknown manifest/row fields.
- Provide no database URL flag or implicit database fallback.
- Reuse `build_event_root_dataset` and `export_event_root_dataset_parquet`.

Database export boundary:

- Extend `research_snapshot_compile` with `--portable-output-dir <dir>` and `--event-horizon-json <canonical-json>`. Both flags are required together; omitting both preserves the existing snapshot-only behavior.
- Reuse the exact `ResearchSnapshot` just built from PostgreSQL for `factor_observations.jsonl`. Query `pm_market_metadata` only through a typed SQLx loader for the corresponding snapshot symbols/start/end, and write one unique `EventMetadataChronologyInput` per observed event with non-null start/end timestamps.
- Require exact event-ID/symbol coverage between snapshot observations and chronology, a source window equal to the snapshot manifest, and one validated horizon whose symbols/window fit that source. Missing, duplicate, extra, ambiguous, or mistimed chronology rows fail before publishing any artifact.
- Write the three portable files through a temporary sibling directory, close and validate them by calling `load_portable_event_root_input`, then atomically rename to the final output. Do not put database URLs, query text, or row data in provenance/log output.
- `research-snapshot.yml` accepts strict `horizon_json` and `orchestrator_action_id` inputs for the portable lane, resolves `git_ref` to exact `origin/main`, runs the CI-built snapshot compiler on the research host, and uploads a second retained artifact named `event-root-portable-input-${{ github.run_id }}` plus SHA-256 provenance. Its existing snapshot artifact remains unchanged.
- Local tests exercise the exporter over deterministic rows/query-result seams only. They never require local PostgreSQL; the workflow is the production PostgreSQL-to-portable bridge.

CLI:

```text
event_root_dataset_producer --input-root <dir> --output-root <dir>
```

Fixture acceptance test:

- Generate 450 deterministic events in test code.
- Write all three portable files into a temporary directory.
- Produce the source event-root dataset.
- Run the existing rolling splitter with 150 events per child.
- Assert at least three child manifests preserve the same horizon.
- Read child `event_index.parquet` files and prove event IDs are pairwise disjoint.
- Commit no JSONL or Parquet fixture data.

Step 1: Add failing tests.

```rust
fn portable_manifest_rejects_unknown_fields()
fn portable_input_rejects_parent_path_escape()
fn portable_input_reports_malformed_jsonl_line()
fn portable_input_rejects_missing_timestamps()
fn portable_fixture_produces_three_disjoint_horizon_safe_windows()
fn snapshot_export_rejects_chronology_coverage_or_horizon_mismatch()
fn snapshot_rows_export_three_files_and_reload_without_database()
fn research_snapshot_workflow_emits_hashed_portable_input_from_postgres_path()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-research --features polars-export --example event_root_dataset_producer
rtk cargo test -p ploy-research --features polars-export --example event_dataset_rolling_windows portable_fixture_produces_three_disjoint_horizon_safe_windows -- --exact
rtk cargo test -p ploy-research --features db,polars-export --example research_snapshot_compile --no-run
rtk cargo test -p ploy-research --features db,polars-export snapshot_export_rejects_chronology_coverage_or_horizon_mismatch --lib -- --exact
rtk cargo test -p ploy-research --features db,polars-export snapshot_rows_export_three_files_and_reload_without_database --lib -- --exact
rtk cargo test --locked -p ploy --test workflow_security research_snapshot_emits_portable_event_root_input -- --exact
```

Expected RED result: the producer target/loader do not exist and the current rolling fixture bypasses portable input.

Step 3: Implement the narrow loader/producer.

- Use `BufRead::lines`; do not load unbounded JSONL into one string.
- Keep filesystem validation in `portable.rs` and dataset validation in the existing builder.
- Print output manifest path, horizon, and event count without printing data rows.
- Keep SQL construction/loading in `research_snapshot.rs`; the examples remain argv parsing plus library calls.

Step 4: Verify.

```bash
rtk cargo test -p ploy-research --features polars-export --example event_root_dataset_producer
rtk cargo test -p ploy-research --features polars-export --example event_dataset_rolling_windows
rtk cargo test -p ploy-research --features polars-export dataset --lib
rtk cargo test -p ploy-research --features db,polars-export --example research_snapshot_compile --no-run
rtk cargo test -p ploy-research --features db,polars-export snapshot_export_rejects_chronology_coverage_or_horizon_mismatch --lib -- --exact
rtk cargo test -p ploy-research --features db,polars-export snapshot_rows_export_three_files_and_reload_without_database --lib -- --exact
rtk cargo test --locked -p ploy --test workflow_security research_snapshot_emits_portable_event_root_input -- --exact
ruby -e 'require "yaml"; YAML.load_file(ARGV.fetch(0))' .github/workflows/research-snapshot.yml
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-research/src/dataset \
  crates/ploy-research/src/research_snapshot.rs \
  crates/ploy-research/src/lib.rs \
  crates/ploy-research/Cargo.toml \
  crates/ploy-research/examples/event_root_dataset_producer.rs \
  crates/ploy-research/examples/event_dataset_rolling_windows.rs \
  crates/ploy-research/examples/research_snapshot_compile.rs \
  .github/workflows/research-snapshot.yml \
  tests/workflow_security.rs
git commit -m "feat(research): produce event-root datasets from portable input"
```

---

### Task 3: Enforce and propagate horizon through the Event ML runner

Files:

- Add `crates/ploy-research/src/event_ml/dataset_contract.rs`.
- Add `crates/ploy-research/src/event_ml/feature_governance.rs`.
- Add `crates/ploy-research/src/event_ml/coverage.rs`.
- Add `crates/ploy-research/src/event_ml/attribution.rs`.
- Add `crates/ploy-research/src/event_ml/baseline.rs`.
- Add `crates/ploy-research/src/event_ml/model_family.rs`.
- Add `crates/ploy-research/src/event_ml/hyperparameter.rs`.
- Add `crates/ploy-research/src/event_ml/workflow.rs`.
- Modify `crates/ploy-research/src/event_ml/mod.rs`.
- Modify `crates/ploy-research/examples/event_dataset_coverage.rs`.
- Modify `crates/ploy-research/examples/event_factor_attribution.rs`.
- Modify `crates/ploy-research/examples/event_dataset_baseline.rs`.
- Modify `crates/ploy-research/examples/event_ml_workflow.rs`.
- Modify `crates/ploy-research/examples/event_ml_rolling_workflow.rs`.

Shared validation:

```rust
pub fn validate_event_ml_dataset_contract(
    manifest: &DatasetBuildManifest,
    entry_secs: i64,
) -> Result<(), String>;

pub fn ensure_same_event_ml_horizon<'a>(
    manifests: impl IntoIterator<Item = &'a DatasetBuildManifest>,
) -> Result<EventHorizonContract, String>;
```

Shared phase APIs extracted from the existing example-local implementations:

```rust
pub fn run_coverage_phase(
    config: &CoveragePhaseConfig,
) -> anyhow::Result<CoverageDiagnosticsArtifact>;

pub fn run_attribution_phase(
    config: &AttributionPhaseConfig,
) -> anyhow::Result<FactorAttributionArtifact>;

pub fn run_feature_governance_phase(
    attribution: &FactorAttributionArtifact,
    config: &FeatureGovernanceConfig,
) -> anyhow::Result<GovernedFeatureSetArtifact>;

pub fn run_baseline_phase(
    config: &BaselinePhaseConfig,
) -> anyhow::Result<BaselinePhaseArtifacts>;

pub fn run_model_family_phase(
    config: &ModelFamilyPhaseConfig,
) -> anyhow::Result<ModelFamilyDecisionArtifact>;

pub fn run_hyperparameter_search_phase(
    config: &HyperparameterSearchPhaseConfig,
) -> anyhow::Result<HyperparameterSearchArtifact>;

pub fn run_event_ml_workflow(
    config: &EventMlWorkflowConfig,
) -> anyhow::Result<EventMlWorkflowArtifacts>;
```

`CoveragePhaseConfig` owns dataset root, entry seconds, tolerances, and feature names. `AttributionPhaseConfig` owns dataset root, entry/tolerance, top-N, whitelist thresholds/max features, and output directory. `BaselinePhaseConfig` owns dataset root, exact governed-feature artifact path/SHA, entry/tolerance, min edge, epochs, learning rate, L2, and output directory. `ModelFamilyPhaseConfig` consumes the fixed baseline and governed-feature hashes. `HyperparameterSearchPhaseConfig` consumes that exact model-family decision, a bounded candidate grid, and train/validation data; it may read test data only after the winning validation candidate is fixed. `run_event_ml_workflow` is the single library orchestrator that writes the existing exact `workflow_report.json`, `model_family_decision.json`, `hyperparameter_search.json`, and selected candidate baseline output expected by `build_walk_forward_report`. Move the current loaders/math/artifact structs and orchestration out of the example without changing algorithms; each example keeps only argv parsing, one library call, and rendering. Tests call the library and never spawn Cargo/examples.

Current Event ML trainer policy:

- Consume settlement-probability lane only.
- Require target `settlement_up` and source `official_polymarket`.
- Require CLI `entry_secs == horizon.entry_offset_secs`.
- A repricing dataset may be produced and diagnosed but cannot be silently passed to the settlement trainer.
- Validate before creating a run directory or spawning any phase binary.

Artifact propagation:

- `coverage_diagnostics.json`
- `factor_attributions.json`
- `event_ml_factor_registry.json`
- `governed_feature_set.json`
- `baseline_metrics.json`
- `model_family_decision.json`
- `hyperparameter_search.json`
- `workflow_report.json`
- `rolling_workflow_report.json`

Every artifact above carries the identical serialized `horizon` object. Replace hard-coded attribution target constants with the manifest target.

Baseline model contract:

```rust
const BASELINE_MODEL_ARTIFACT_VERSION: u32 = 2;

struct BaselineModelArtifact<'a> {
    kind: &'static str,
    version: u32,
    family: &'static str,
    horizon: &'a EventHorizonContract,
    target_label: &'a str,
    // existing feature schema, standardizer, intercept, and weights
}
```

Add the missing model-family phase without adding a model implementation:

```text
coverage
-> attribution
-> feature_governance
-> baseline
-> model_family
-> hyperparameter
-> walk_forward
```

`governed_feature_set.json` is a versioned, sorted allowlist derived only from train/validation attribution evidence. It records each accepted/rejected feature and reason, the source attribution SHA-256, horizon, target, and schema hash. Baseline and every later phase require its exact SHA-256 and may not add a feature ad hoc. `model_family_decision.json` then selects the already implemented logistic regression and records why DL/RL stay blocked.

Step 1: Add failing tests.

```rust
fn event_ml_rejects_repricing_dataset()
fn event_ml_rejects_entry_offset_mismatch()
fn rolling_workflow_rejects_mixed_300_900_contracts_before_spawn()
fn workflow_report_carries_horizon()
fn default_phase_order_includes_governance_and_model_family_before_search()
fn baseline_rejects_missing_or_mismatched_governed_feature_set()
fn baseline_model_contract_carries_horizon_and_version_2()
fn library_workflow_materializes_exact_walk_forward_input_contract()
fn hyperparameter_selection_never_uses_test_metrics()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-research --features polars-export --example event_ml_workflow
rtk cargo test -p ploy-research --example event_ml_rolling_workflow
rtk cargo test -p ploy-research --features polars-export --example event_dataset_baseline
```

Expected RED result: entry/horizon mismatch is accepted, reports lack horizon, and the runner has no governed feature artifact before baseline/search.

Step 3: Implement validation and propagation.

- Parse the manifest once per dataset and pass the validated contract through phase artifacts.
- Make coverage, attribution, and baseline examples thin wrappers over the shared APIs above; the portable integration test calls the same APIs directly.
- Make model-family selection, bounded hyperparameter search, candidate-baseline materialization, and workflow-report writing library-owned as well. The example must not retain a second artifact shape or selection implementation.
- Keep train-only standardization and validation-only candidate selection unchanged.
- Count the model-family decision as a recorded hypothesis decision.
- Make feature governance a real executable phase and gate, not a runbook-only label.

Step 4: Verify.

```bash
rtk cargo test -p ploy-research --features polars-export --example event_ml_workflow
rtk cargo test -p ploy-research --example event_ml_rolling_workflow
rtk cargo test -p ploy-research --features polars-export --example event_dataset_coverage
rtk cargo test -p ploy-research --features polars-export --example event_factor_attribution
rtk cargo test -p ploy-research --features polars-export --example event_dataset_baseline
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-research/src/event_ml \
  crates/ploy-research/examples/event_dataset_coverage.rs \
  crates/ploy-research/examples/event_factor_attribution.rs \
  crates/ploy-research/examples/event_dataset_baseline.rs \
  crates/ploy-research/examples/event_ml_workflow.rs \
  crates/ploy-research/examples/event_ml_rolling_workflow.rs
git commit -m "feat(event-ml): enforce horizon before training"
```

---

### Task 4: Block cross-horizon walk-forward, handoff, and runtime scoring

Files:

- Modify `crates/ploy-research/src/event_ml/walk_forward.rs`.
- Modify `crates/ploy-research/examples/event_ml_walk_forward.rs`.
- Modify `crates/ploy-strategy-bundles/src/strategies/event_ml_model.rs`.
- Modify `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`.
- Modify `scripts/apply_event_ml_handoff_to_config.py`.
- Modify `scripts/build_runtime_candidate_strategy_replay.py`.
- Modify `tests/test_apply_event_ml_handoff_to_config.py`.
- Modify `tests/test_build_runtime_candidate_strategy_replay.py`.
- Modify `tests/test_strategy_config_contracts.py`.
- Add `crates/ploy-research/tests/event_root_portable_input.rs`.
- Modify `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`.
- Modify `config/strategies/02-pm5d-threelayer.repricing-momentum-dryrun.toml`.
- Read but do not modify `config/strategies/02-pm5d-threelayer.live.toml`; prerequisite Live Task 2 explicitly changes it to `[300]`, and this slice's contract test fails if that prerequisite was not merged.
- Modify `config/deployments/README.md`.

Walk-forward rules:

- Workflow and selected baseline artifact within one run must have identical horizon.
- All supplied run directories must have identical horizon.
- Mixed horizon is an error before report/handoff generation, not merely a blocked candidate.
- `WalkForwardReport`, each window, and handoff carry the horizon.
- At least three distinct dataset windows remain required.

Correct the promotion order:

- Add `--candidate-replay-json <path>` to the walk-forward/handoff gate.
- Pre-dry-run handoff requires `basis=runtime_market_update_replay`, `promotion_ready=true`, no blocking risk flags, exact runtime score match, and exact horizon match.
- Recorded replay/dry-run parity is a post-dry-run, pre-live gate. It cannot replace the historical executable candidate replay.
- Missing candidate replay produces blocker `candidate_replay_missing`.
- The candidate replay builder must emit and gate finite `executable_cost`, quantity-weighted `average_entry`, trade-level chronological `max_drawdown`, bankroll, fees, slippage, and latency assumptions. Compute cost from actual entry fills, average entry from fill quantity/notional, and drawdown from the deterministically ordered official-settlement equity curve; never substitute configured stake or row count when fill evidence exists.
- `promotion_ready=true` is impossible when executable cost is absent/non-positive, average entry is outside `[0,1]`, maximum drawdown/bankroll is absent or non-finite, cost assumptions are missing, one event has multiple entry decisions, or any event lacks official payout. The handoff parser independently revalidates these fields and exact horizon/runtime-score/config/model/runner hashes.

Runtime model rules:

- Model artifact V2 uses the shared `EventHorizonContract`.
- Governed profiles `settlement_probability` and `repricing_momentum` require exactly one `allowed_window_secs` value.
- That value must be 300 or 900 and match the model horizon.
- Model allowed symbols must match the config symbol set.
- Runtime event window mismatch increments `skip_event_ml_horizon_mismatch` and emits no intent.

Current PM5D configs become:

```toml
allowed_window_secs = [300]
```

Do not create a copied PM15D model/config. Document the required future 900-second names and generate them only from an independent ready handoff, with a distinct deployment ID, model path, recording path, replay artifact, and trace profile.

Step 1: Add failing tests.

```rust
fn walk_forward_rejects_mixed_horizons()
fn handoff_requires_matching_executable_replay()
fn recorded_parity_does_not_replace_candidate_replay()
fn event_ml_model_rejects_pm1h_horizon()
fn event_ml_runtime_rejects_model_window_mismatch()
fn governed_profiles_require_one_supported_window()
fn runtime_window_mismatch_emits_no_intent()
fn portable_fixture_runs_coverage_attribution_governance_baseline_and_walk_forward()
fn candidate_replay_requires_cost_average_entry_drawdown_and_assumptions()
```

Python:

```text
test_blocks_handoff_window_mismatch
test_blocks_multi_window_config
test_accepts_matching_single_window_config
candidate_replay_missing_executable_metrics_is_never_promotion_ready
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-research --example event_ml_walk_forward
rtk cargo test -p ploy-research --features polars-export --test event_root_portable_input
rtk cargo test -p ploy-strategy-bundles strategies::event_ml_model
rtk cargo test -p ploy-strategy-bundles governed_profiles_require_one_supported_window
rtk pytest tests/test_apply_event_ml_handoff_to_config.py -q
```

Expected RED result: mixed windows can be aggregated and `[300, 900]` configs are accepted.

Step 3: Implement the three matching gates.

- Create `crates/ploy-research/tests/` before adding the portable integration test.
- Validate dataset-to-model, model-to-config, and config-to-runtime-event.
- In the integration test, generate the same 450 portable events from Task 2, create three disjoint 150-event child datasets, and call `run_event_ml_workflow` on each exact child. That shared library path must execute coverage, attribution, feature governance, fixed baseline, model-family decision, and bounded hyperparameter selection and must materialize the exact artifacts consumed by `build_walk_forward_report`. Then call `build_walk_forward_report` directly over the three run directories. Assert every artifact carries one identical horizon, baseline/search consume the governed feature SHA, the validation-selected candidate is fixed before test metrics are read, and walk-forward has at least three distinct windows. The fixture remains diagnostic and makes no profitability assertion.
- Do not duplicate example internals, hand-write fake `workflow_report.json`/`hyperparameter_search.json`, or spawn Cargo/examples from the test.
- Keep the scorer pure and reuse the same contract parser in replay and runtime.
- Do not loosen a gate to keep an old artifact promotable; old V1 artifacts remain diagnostic only.

Step 4: Verify.

```bash
rtk cargo test -p ploy-research --example event_ml_walk_forward
rtk cargo test -p ploy-research --features polars-export --test event_root_portable_input
rtk cargo test -p ploy-strategy-bundles strategies::event_ml_model
rtk cargo test -p ploy-strategy-bundles three_layer
rtk pytest tests/test_apply_event_ml_handoff_to_config.py -q
rtk pytest tests/test_build_runtime_candidate_strategy_replay.py -q
rtk pytest tests/test_strategy_config_contracts.py -q
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-research/src/event_ml/walk_forward.rs \
  crates/ploy-research/examples/event_ml_walk_forward.rs \
  crates/ploy-research/tests/event_root_portable_input.rs \
  crates/ploy-strategy-bundles/src/strategies/event_ml_model.rs \
  crates/ploy-strategy-bundles/src/strategies/three_layer.rs \
  scripts/apply_event_ml_handoff_to_config.py \
  scripts/build_runtime_candidate_strategy_replay.py \
  tests/test_apply_event_ml_handoff_to_config.py \
  tests/test_build_runtime_candidate_strategy_replay.py \
  tests/test_strategy_config_contracts.py \
  config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml \
  config/strategies/02-pm5d-threelayer.repricing-momentum-dryrun.toml \
  config/deployments/README.md
git commit -m "fix(research): isolate model and handoff by horizon"
```

---

### Task 5: Add the artifact-only producer workflow and pre-training horizon gate

Files:

- Add `.github/workflows/event-root-dataset-producer.yml`.
- Add `.github/workflows/event-ml-config-pr.yml`.
- Modify `.github/workflows/event-ml-rolling-evidence.yml`.
- Modify `tests/workflow_security.rs`.
- Add `tests/test_event_ml_rolling_workflow.py`.

Producer workflow inputs:

```yaml
git_ref:                    # must resolve to exact origin/main for retained evidence
source_input_run_id:        # required
source_input_artifact_name: # optional; default event-root-portable-input-<run-id>
```

Producer behavior:

- Run on GitHub-hosted Ubuntu.
- Download a portable-input artifact containing the three required files.
- Build `event_root_dataset_producer` in CI.
- Produce under `source_event_root/`.
- Upload `event-ml-rolling-datasets-${{ github.run_id }}`.
- Write provenance containing source run, artifact name, checked-out SHA, manifest version, and full horizon object.
- Do not reference a DB URL, SSH, or a self-hosted runner.

Rolling workflow options:

```json
{
  "expected_market_window_secs": "300",
  "expected_target_label": "settlement_up"
}
```

Add a step named `Validate source dataset horizon before split` that uses Python standard library to reject before build/training when:

- manifest version is not 2;
- expected window or target differs;
- workflow entry seconds differ from `entry_offset_secs`;
- requested symbols are outside allowed symbols;
- source window falls outside dispatch start/end;
- window is 3600;
- required horizon fields are absent.

Dedicated config PR gate:

- Remove/disable config mutation from `event-ml-rolling-evidence.yml`; training always emits evidence and never opens a PR. Unknown/deprecated `create_config_pr=true` input fails closed rather than being silently honored.
- `event-ml-config-pr.yml` accepts only exact source handoff run/artifact, candidate replay run/artifact, canonical expected horizon, and `orchestrator_action_id`. It checks both successful `workflow_dispatch` runs used exact `main` SHA, downloads by exact names, verifies artifact SHA-256/provenance, and rejects metadata-only or cross-run evidence.
- Parse the final handoff into a dedicated `config_pr_ready` output. A PR is allowed only when handoff status is ready, recommended action is `promote_to_runtime`, blocker list is empty, candidate replay is promotion-ready with all executable metrics, and runtime score, full horizon, config/model/runner hashes, candidate ID, and main SHA match exactly.
- Choose the destination inside the workflow from a hard-coded horizon map; the caller cannot supply a path. PM5D may update only the named dry-run config. PM15D may create its separately named dry-run config/model reference only from an independent 900-second handoff. PM1H and every live config path are rejected.
- Invoke `apply_event_ml_handoff_to_config.py`, commit only the hard-coded dry-run path plus a machine-readable evidence receipt, and open a review-required PR. The workflow never merges, deploys, resumes, writes a live config, or uses a trade environment.
- A blocked/missing/mismatched handoff exits before checkout mutation. Both the workflow and the parent Research Agent adapter revalidate the exact typed evidence; the model cannot set a boolean to bypass the gate.

Step 1: Add failing workflow tests.

```rust
fn event_root_dataset_producer_is_artifact_only()
fn event_root_dataset_producer_has_no_database_or_ssh_fallback()
fn event_ml_rolling_evidence_validates_horizon_before_training()
fn event_ml_rolling_evidence_rejects_pm1h()
fn event_ml_rolling_summary_includes_full_horizon()
fn event_ml_config_pr_step_requires_ready_dry_run_handoff()
fn event_ml_training_workflow_cannot_mutate_config()
fn event_ml_config_pr_uses_hard_coded_dry_run_target_and_never_deploys()
```

Python behavior tests:

```text
blocked_handoff_cannot_create_config_pr
ready_matching_handoff_can_prepare_only_dry_run_config_pr
agent_options_cannot_enable_config_pr_on_training_workflow
dedicated_config_pr_rejects_mismatched_or_incomplete_executable_evidence
```

Step 2: Run RED.

```bash
rtk cargo test --locked -p ploy --test workflow_security event_root_dataset_producer_is_artifact_only -- --exact
rtk cargo test --locked -p ploy --test workflow_security event_ml_rolling_evidence_validates_horizon_before_training -- --exact
rtk pytest tests/test_event_ml_rolling_workflow.py -q
```

Expected RED result: producer workflow is absent and rolling training never validates horizon.

Step 3: Add workflows and fail-closed parsing.

- Reuse `scripts/download_github_artifact.py`.
- Keep `options_json` unknown-key rejection.
- Do not dispatch either workflow locally.

Step 4: Verify syntax and contracts.

```bash
rtk cargo test --locked -p ploy --test workflow_security event_root_dataset_producer -- --nocapture
rtk cargo test --locked -p ploy --test workflow_security event_ml_rolling_evidence -- --nocapture
ruby -e 'require "yaml"; ARGV.each { |path| YAML.load_file(path) }' \
  .github/workflows/event-root-dataset-producer.yml \
  .github/workflows/event-ml-config-pr.yml \
  .github/workflows/event-ml-rolling-evidence.yml
if command -v actionlint >/dev/null 2>&1; then
  actionlint .github/workflows/event-root-dataset-producer.yml .github/workflows/event-ml-config-pr.yml .github/workflows/event-ml-rolling-evidence.yml
else
  ruby -e 'require "yaml"; ARGV.each { |path| YAML.load_file(path) }' \
    .github/workflows/event-root-dataset-producer.yml \
    .github/workflows/event-ml-config-pr.yml \
    .github/workflows/event-ml-rolling-evidence.yml
fi
rtk git diff --check
```

Step 5: Commit.

```bash
git add .github/workflows/event-root-dataset-producer.yml \
  .github/workflows/event-ml-config-pr.yml \
  .github/workflows/event-ml-rolling-evidence.yml \
  tests/workflow_security.rs \
  tests/test_event_ml_rolling_workflow.py
git commit -m "ci(research): produce and validate horizon-safe datasets"
```

---

### Task 6: Update evidence templates and run full research acceptance

Files:

- Modify `docs/runbooks/event-ml-automl-workflow.md`.
- Modify `tasks/research_evidence/TEMPLATE.md`.
- Modify `config/deployments/README.md`.
- Modify `tasks/todo.md` with exact verification results.

Evidence template fields:

```text
Market window seconds
Prediction horizon seconds
Entry offset seconds
Target label
Accounting lane
Settlement source
Allowed symbols
Dataset manifest/run/artifact ID
Distinct walk-forward window count
Executable costs and maximum drawdown
Decision: continue, revise, reject, or promote to dry-run
```

Runbook rules:

- PM5D and PM15D are separate artifact/model/config/tape/trace lanes.
- PM1H is unsupported until discovery, labels, data retention, replay accounting, and profile work exist.
- The Event ML trainer currently consumes settlement lane only.
- Canonical order is coverage, attribution, feature governance, fixed baseline, model-family decision, bounded search, at least three distinct walk-forward windows, executable replay, dry-run, then recorded parity.
- Historical executable replay is required before dry-run; recorded replay/dry-run parity is required before live.
- Entry-grid rows from one event are diagnostics, not independent trades.

Final commands:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked -p ploy-operator-contracts
rtk cargo test --locked -p ploy-research --features polars-export
rtk cargo test --locked -p ploy-strategy-bundles
rtk pytest tests/test_apply_event_ml_handoff_to_config.py -q
rtk pytest tests/test_strategy_config_contracts.py -q
rtk cargo test --locked -p ploy --test workflow_security
rtk cargo test --locked --workspace
rtk git diff --check
```

Expected result: all commands pass; the 450-event fixture yields at least three disjoint children; no database, wallet, order, redeem, workflow dispatch, or cloud operation occurs.

Commit:

```bash
git add docs/runbooks/event-ml-automl-workflow.md \
  tasks/research_evidence/TEMPLATE.md \
  config/deployments/README.md \
  tasks/todo.md
git commit -m "docs(research): record horizon-safe evidence contract"
```

## Completion Criteria

- Dataset manifest V2 carries exactly one validated horizon.
- Mixed 300/900 events fail before split/training.
- PM1H fails with an explicit governed-profile error.
- The research snapshot workflow exports PostgreSQL truth into a hashed three-file portable artifact; the hosted dataset producer then creates canonical Parquet with no DB fallback.
- A deterministic fixture produces at least three disjoint child datasets.
- Horizon propagates through coverage, attribution, model, search, walk-forward, replay, and handoff.
- Shared library APIs, not example subprocesses or fabricated files, execute model-family selection and bounded search before the three-window walk-forward test.
- PM5D config accepts only 300 seconds; no copied PM15D scorer/config is fabricated.
- Executable replay precedes dry-run; recorded parity remains pre-live.
- A dedicated evidence-gated workflow can prepare only a review-required dry-run config PR; training itself and the Research Agent cannot write live config or deploy.
- No strategy profitability or deployability claim is made from fixture evidence.
