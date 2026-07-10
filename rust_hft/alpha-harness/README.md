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

The command calls the registered public connector, writes a content-addressed JSONL trace and manifest, persists quality evidence, and writes a failure artifact on acquisition failure. It never substitutes fixtures.

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

Supported engines are `gp`, `mcts`, `bayesian`, `offline-rl`, and `llm`. Offline RL requires an explicit trace file and minimum history. LLM requires:

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

Only a walk-forward Keep candidate can access the sealed holdout. Repeated failures generate one idempotent follow-up mission and learning directive. Add `--llm-critic` for a bounded real failure explanation.

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

For paper/shadow runtime intake, first obtain current hashes:

```bash
cargo run -p hft-live --no-default-features -- \
  --config config/dev/binance_quotes_only.yaml \
  --deployment-hashes-only
```

Then start `hft-live` with the signed envelope, runtime policy, trusted public keys, nonce ledger, audit log, and feedback log. Runtime verification occurs before startup configuration changes.
