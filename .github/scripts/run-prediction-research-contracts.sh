#!/usr/bin/env bash
# Ephemeral CI fixtures only; no research experiment or cloud control.
set -euo pipefail
cd "$(dirname "$0")/../../rust_hft/prediction-markets"
cargo test --locked -p ploy-feed-loaders --lib
cargo test --locked -p ploy-research --lib
cargo test --locked -p ploy-research --features ml --lib
cargo clippy --locked -p ploy-research --features ml --all-targets --no-deps -- -D warnings
cargo check --locked -p ploy-research --bin monday-prediction-research
cargo check --locked -p ploy-research --features db --bin monday-prediction-evaluator
cargo test --locked -p ploy-market-data --features audit --lib --examples
cargo test --locked -p ploy-research --features db --bin monday-prediction-snapshot
handoff_root="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/prediction-evidence-handoff.XXXXXX")"
cleanup_handoff() {
  find "$handoff_root" -depth -type f -delete
  find "$handoff_root" -depth -type d -empty -delete
}
trap cleanup_handoff EXIT
export PLOY_TEST_EVIDENCE_HANDOFF="$handoff_root"
cargo test --locked -p ploy-research --features db --test polymarket_three_event_e2e \
  producer_snapshot_smoke_and_three_task_receipts_share_one_partition -- --exact --nocapture
test -s "$handoff_root/refs.json"
CODEX_CLI_BIN="$handoff_root/mock-codex" CODEX_CLI_TIMEOUT_SECS=10 \
  cargo test --locked -p ploy-agent-sidecar --lib \
  tests::production_writer_evidence_queue_reaches_terminal \
  -- --exact --ignored --test-threads=1 --nocapture
test -s "$handoff_root/consumer-proof.json"

cargo install sqlx-cli \
  --locked \
  --version "${SQLX_CLI_VERSION}" \
  --no-default-features \
  --features rustls,postgres

sqlx migrate run
set -euo pipefail
SECONDS=0
cargo check --locked -p ploy-research --features db,polars-export,ml,rl,strategy-runtime --lib
cargo test --locked -p ploy-research --features db --example persist_research_trace
cargo test --locked -p ploy-research --features db --example persist_research_trace \
  tests::postgres_factor_registry_keeps_pooled_up_down_identity_distinct \
  -- --ignored --exact
cargo test --locked -p ploy-research --features db --example research_trace_plan
cargo test --locked -p ploy-research --features db --example research_trace_plan \
  tests::postgres_legacy_manager_excludes_side_bound_trace_and_evaluation \
  -- --ignored --exact
cargo test --locked -p ploy-research --features db --lib -- --ignored --list \
  | grep -F 'research_snapshot::tests::postgres_official_outcomes_use_token_primary_keys_and_version_clocks: test'
cargo test --locked -p ploy-research --features db \
  research_snapshot::tests::postgres_official_outcomes_use_token_primary_keys_and_version_clocks \
  --lib -- --ignored --exact
cargo test --locked -p ploy-market-data --features live \
  predict_fun::tests::postgres_predict_fun_restart_restores_clock_and_persists_failure_readiness \
  --lib -- --ignored --exact
echo "### Rust research heavy lane" >> "$GITHUB_STEP_SUMMARY"
echo "Elapsed seconds: ${SECONDS}" >> "$GITHUB_STEP_SUMMARY"
