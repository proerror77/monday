# Alpha Harness
Rust CLI and libraries for governed Agentic Alpha research. It has no order or trade command and does not depend on execution adapters.

## Packages

- `alpha-domain`: typed missions, candidates, feedback, learning, approvals, and signed envelopes.
- `alpha-store`: DuckDB source of truth with content hashes and append-only journals.
- `alpha-engine`: GP, MCTS, Bayesian search, offline Q-learning, OpenAI-compatible proposals/critics, causal DSL evaluation, resume, and learning.
- `alpha-harness`: Agent-facing CLI.

## Data Mission

```bash
cargo run -p alpha-harness -- data sources

cargo run -p alpha-harness -- data acquire \
  --db var/alpha.duckdb \
  --mission-id data-btc-1 \
  --symbol BTCUSDT \
  --interval 1m \
  --limit 500 \
  --artifact-dir var/datasets
```

The command calls the registered public connector, writes a content-addressed JSONL trace and manifest, persists the full manifest as an immutable DuckDB registry revision, and writes a failure artifact on acquisition failure. Research commands reject a disk manifest that does not exactly match that revision. The acquisition path never substitutes fixtures.

## Research Mission

Create a JSON `ResearchMission`, then run one explicitly selected engine:

```bash
cargo run -p alpha-harness -- mission create \
  --db var/alpha.duckdb --mission mission.json

cargo run -p alpha-harness -- mission run \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --engine gp \
  --dataset-manifest var/datasets/dataset-....manifest.json

cargo run -p alpha-harness -- mission status \
  --db var/alpha.duckdb --mission-id mission-1

cargo run -p alpha-harness -- candidate list \
  --db var/alpha.duckdb --mission-id mission-1
```

Supported engines are `gp`, `mcts`, `bayesian`, `offline-rl`, and `llm`. Offline RL requires an explicit trace file and minimum history; its output is lab search-policy evidence and cannot access sealed holdout or promotion authority. LLM requires:

```text
ALPHA_LLM_ENDPOINT
ALPHA_LLM_API_KEY
ALPHA_LLM_MODEL
ALPHA_LLM_PROVIDER  # optional
```

Credentials are process inputs and are never persisted.

## Evaluation and Learning

```bash
cargo run -p alpha-harness -- evaluate \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --candidate-id candidate-1 \
  --dataset-manifest var/datasets/dataset-....manifest.json

cargo run -p alpha-harness -- mission learn \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --repeated-failure-threshold 3
```

Only a canonical Formula v2 walk-forward Keep candidate can access the sealed holdout. The evaluator persists rows, trades, post-cost edge, drawdown, raw score, adjusted score, config, and version; config and metrics hashes are bound into promotion and bundle hashes. A mission may pre-register a larger multiple-testing family through `validator_spec.multiple_testing_trials`, but it cannot declare fewer trials than its candidate budget. Repeated failures generate one idempotent follow-up mission and learning directive. Add `--llm-critic` for a bounded real failure explanation.

Runtime feedback and search policy revisions enter through typed JSON:

```bash
cargo run -p alpha-harness -- feedback ingest --db var/alpha.duckdb --record feedback.json
cargo run -p alpha-harness -- policy propose --db var/alpha.duckdb --record policy.json
```

A child search policy is adopted only when its validator score strictly beats its adopted parent.

## Signed Handoff

```bash
cargo run -p alpha-harness -- deployment scope-hash --envelope envelope.json
cargo run -p alpha-harness -- approval record --db var/alpha.duckdb --record approval.json
cargo run -p alpha-harness -- deployment sign \
  --db var/alpha.duckdb \
  --envelope envelope.json \
  --signing-key signing-key.hex \
  --key-id research-signer-1 \
  --output signed-envelope.json
```

The signing key file contains exactly 32 bytes as hex and is never stored in DuckDB. Live-small signing requires a persisted `human_live_small` approval whose `scope_hash` matches venue, instruments, and live-small intent. The runtime still refuses live-small activation until universal per-order limits are implemented.

The runtime bundle schema can validate Formula and ONNX artifacts, but the current governed evaluator/promotion producer emits Formula bundles only. ONNX candidates remain research-only until a point-in-time ONNX evaluator and training lineage are implemented; runtime ONNX compatibility tests are not evidence of governed ONNX promotion.

For paper/shadow runtime intake, first obtain current hashes:

```bash
cargo run -p hft-live --no-default-features -- \
  --config config/dev/binance_quotes_only.yaml \
  --deployment-hashes-only
```

Then start `hft-live` with the signed envelope, runtime policy, trusted public keys, nonce ledger, audit log, and feedback log. Runtime verification occurs before startup configuration changes.
