# Alpha Harness
Rust CLI and libraries for the governed, bounded Loop Engineer research plane. It has no order or trade command and does not depend on execution adapters.

## Packages

- `alpha-domain`: typed missions, candidates, feedback, learning, approvals, and signed envelopes.
- `alpha-store`: DuckDB source of truth with content hashes and append-only journals.
- `alpha-engine`: GP, MCTS, Bayesian search, offline Q-learning, OpenAI-compatible proposals/critics, causal DSL evaluation, checkpointing, and learning.
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

## Bounded LoopRun

Run or resume a durable staged goal with the same command:

```bash
cargo run -p alpha-harness -- loop run \
  --db var/alpha.duckdb \
  --loop-run-id loop-btc-1 \
  --mission-id mission-1 \
  --engine mcts \
  --dataset-manifest var/datasets/dataset-....manifest.json \
  --target-stage shadow-healthy \
  --max-research-missions 2

cargo run -p alpha-harness -- loop status \
  --db var/alpha.duckdb \
  --loop-run-id loop-btc-1
```

The durable LoopRun accepts only `mcts` or `bayesian`, the two engines with versioned exact engine-state checkpoints. GP, offline RL, and LLM remain available through standalone `mission run` commands but cannot claim exact LoopRun resume. The LoopRun records ordered stages, completion policy, child missions, and an explicit stop reason. Missing evaluation, holdout, Paper, Shadow, or human evidence pauses the loop instead of fabricating progress. An external scheduler may invoke this command, but invocation does not bypass stage evidence.

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
cargo run -p alpha-harness -- feedback ingest \
  --db var/alpha.duckdb \
  --record signed-feedback.json \
  --trusted-keys runtime-feedback-trusted-keys.json
cargo run -p alpha-harness -- policy propose --db var/alpha.duckdb --record policy.json
```

Feedback records and JSONL logs must contain `SignedRuntimeAttributionEvent` wrappers. The CLI verifies every content hash, key id, and Ed25519 signature before opening DuckDB; raw or tampered runtime events fail closed. A child search policy is adopted only when its validator score strictly beats its adopted parent.

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

The deployment signing key file contains exactly 32 bytes as hex and is never stored in DuckDB or passed to the runtime. Live-small signing requires a persisted `human_live_small` approval whose `scope_hash` matches venue, instruments, and live-small intent. Runtime-owned `policy.json` must independently carry the referenced approval id, class, promotion subject, scope hash, signer, validity window, and revocation state; an envelope signer cannot self-assert an approval id. The runtime still refuses live-small activation until universal order/slippage enforcement and real-venue reconciliation/reduce-only acceptance tests pass.

ONNX candidates enter through `candidate register-onnx`. The command verifies a bundle-relative model, checksum, byte length, static LOB tensor schema, preprocessing version, registered PIT feature matrix, and Rust walk-forward results before persisting one immutable candidate iteration. Sealed evaluation reruns the same model through Rust. ONNX promotion requires `--bundle-out` and `--model-root`; it materializes the exact verified model beside the content-addressed bundle. Python may train/export the model, but cannot create promotion evidence or load it into the runtime directly.

For paper/shadow runtime intake, first obtain current hashes:

```bash
cargo run -p hft-live --no-default-features -- \
  --config config/dev/binance_quotes_only.yaml \
  --deployment-hashes-only
```

Then start `hft-live` with the signed envelope, runtime policy, trusted deployment public keys, nonce ledger, audit log, feedback log, and a separate runtime feedback signing key. `policy.json` is runtime-owned and read-only; its `approvals` array is the independent approval authority. Formula and ONNX files must be relative to and contained by the bundle directory. Runtime verification occurs before startup configuration changes.
