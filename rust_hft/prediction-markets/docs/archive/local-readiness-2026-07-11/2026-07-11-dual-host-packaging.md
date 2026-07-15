# Dual-Host Release Packaging Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task.

Goal: Prepare mutually exclusive research/data and trade release bundles plus fail-closed GitHub workflows so two future Alibaba Cloud hosts can be installed from immutable CI artifacts without building Rust or embedding long-lived secrets on-host.

Architecture: Keep `deploy-tango-1-1.yml` as the research/data owner, replace the legacy platform release with required production workflow `release-aliyun.yml` as the sole complete trade-release owner, construct both bundles from explicit allowlists, and activate releases through atomic SHA directories/symlinks with tested rollback.

Tech Stack: GitHub Actions, Rust workflow-security tests, Python 3 standard library, Bash, systemd unit files, SHA-256, GitHub CLI, and existing Ploy binaries/configs.

## Global Constraints

- Start after live safety, V2 settlement, horizon-safe research, and Research Orchestrator programs land so packaging tests the final capability boundary.
- Do not create or modify ECS, RDS, OSS, KMS, RAM, security groups, DNS, GitHub environments, secrets, remote files, or services.
- Do not dispatch any deploy, migration, backtest, or research workflow during local implementation.
- Do not build Rust on a trading host. CI builds Linux release binaries and the host only verifies/installs them.
- Do not interpolate wallet, database-password, OSS-secret, SSH-key, or cloud-secret values into persisted/uploaded scripts or bundles.
- Do not use mutable image/artifact tags or `latest`.
- Keep live manifests paused and unrendered until a later protected environment supplies the real normalized wallet address.
- Keep migrations out of the trade role.
- Preserve `deploy-tango-1-1.yml` for research/data. The production trade owner is `.github/workflows/release-aliyun.yml` as required by `AGENTS.md`/`CLAUDE.md`; remove `release-platform.yml` and `deploy-trade.yml` after updating active references. Use `tango-1-1` for research/data and `ploy-trade-1` for trade.
- No workflow is deployment-eligible until Task 9 acceptance. Intermediate commits may extend the packager allowlist as their owned assets appear, but deploy/install jobs stay guarded off until the final role manifest, activator, verifier, units, and tests are present.
- Each task is one atomic commit and stages only its owned paths.

## Role Contract

Research/data bundle allowlist:

- a dedicated `ploy-collector` binary built from `new-ploy-runner` with `--no-default-features --features ops`; its entrypoint exposes collector/check-db commands only and has no strategy-run or live-execution feature;
- research snapshot compiler with PostgreSQL-to-portable export, research trace writer/planner, Event ML/event-root producers, and CI-built `sqlx`;
- compiled `ploy-sidecar/dist` and a dedicated `ploy-research-orchestrator.service`; PM5D/PM15D run contracts arrive inside typed PostgreSQL queue envelopes, so no unowned static profile file is packaged. Host runtime requirements are Node.js 22+ and GitHub CLI, verified before activation;
- collector scripts and research-only systemd units;
- complete migrations directory;
- no `ployd`, `ployctl`, `ploytui`, normal strategy runner, wallet private key material, strategy/deployment manifest, live gate, or trade-host service/timer. Paper/dry-run runtime parity remains a CI/trade-host activity, not a reason to place the execution control plane on the research host.

Research systemd allowlist is the ten Ploy research services listed in Task 3, the new `ploy-quote-collector.service`, the new `ploy-research-orchestrator.service`, plus `ploy-orderbook-snapshot-archive.timer`, `ploy-orderbook-snapshot-retention.timer`, and `ploy-polymarket-v2-indexer-import.timer`. The standalone `polymarket-v2-indexer.service` is excluded until its external Envio application is packaged as a separately verified dependency.

Trade bundle allowlist:

- `ployd`, `ployctl`, `ploytui`, and archive destination `bin/ploy-runner`; that destination is an explicit compatibility alias whose sole allowed source is the separately built `target/.../ploy-trade-runner` ELF, never the legacy combined `ploy-runner` binary;
- platform service, maintenance, watchdog, activation, and verification scripts;
- explicitly named live config plus paused live manifest template and paper drill config;
- `live_dry_run.sh`, `pm5d_threelayer_live_gate.sh`, and `validate_live_promotion_gate.py`;
- no collector, research binary, migration, research workflow helper, or database-migration capability.

Exact trade units/timers are `deployment/ployd.service`, `deployment/ploy-maintenance.service`, `deployment/ploy-maintenance.timer`, `deployment/ploy-platform-watchdog.service`, and `deployment/ploy-platform-watchdog.timer`. Package tests assert every one is present and reject any research systemd unit.

---

## Contract Test Allocation

Do not create a standalone commit containing tests that are intentionally red. Each implementation task adds its own failing contract first, observes RED, implements the matching behavior, reruns GREEN, and commits test plus implementation together:

- Task 1 owns the packager engine, initial role manifests, the ops-only/trade-only entrypoints, deterministic hashes, and synthetic-fixture allowlist tests. Later tasks extend the versioned role manifests/tests only when their owned files exist.
- Task 2 owns Tango/research-only workflow and secret-free Cloud Assistant payload tests.
- Task 3 owns external database collector-unit tests.
- Task 4 owns the sole trade workflow, same-SHA Test provenance, no-host-build, and fresh-host postflight tests.
- Task 5 owns immutable activation/rollback tests.
- Task 6 owns role/env/secret separation tests.
- Task 7 owns legacy host identity and pinned SSH tests.
- Task 8 owns migration workflow tests.
- Task 9 runs and records the complete green matrix.

---

### Task 1: Add one deterministic role-aware bundle packager

Files:

- Add `scripts/ci/package_deploy_bundle.py`.
- Add `scripts/tests/test_package_deploy_bundle.sh`.
- Add `deployment/bundles/research.json` and `deployment/bundles/trade.json` as sorted explicit source/destination manifests.
- Modify `apps/new-ploy-runner/Cargo.toml`.
- Add `apps/new-ploy-runner/src/bin/ploy-collector.rs`.
- Add `apps/new-ploy-runner/src/bin/ploy-trade-runner.rs`.
- Modify `crates/ploy-runner-host/src/lib.rs` to expose explicit ops-only and trade-only entrypoints with process exit status.

CLI:

```text
python3 scripts/ci/package_deploy_bundle.py \
  --role research|trade \
  --repo-root <checkout> \
  --target-dir <target/release> \
  --output-dir <dist> \
  --git-sha <40-hex> \
  --source-date-epoch <integer>
```

Outputs:

```text
ploy-<role>-<sha>.tar.gz
ploy-<role>-<sha>.tar.gz.sha256
ploy-<role>-<sha>/release.json
```

`release.json` schema:

```json
{
  "schema_version": "ploy-release.v1",
  "role": "trade",
  "git_sha": "40-character-sha",
  "source_date_epoch": 0,
  "binary_sources": {"bin/ploy-runner": "ploy-trade-runner"},
  "files": {"relative/path": "sha256"}
}
```

Implementation rules:

- Define explicit versioned per-role file/binary manifests; never glob the checkout into a bundle. Task 1 tests use a complete synthetic source tree, including later capability-slot filenames, so this commit never requires files from a future task. Invoking against a real checkout still fails on any missing manifest source.
- Add `ops = ["ploy-runner-host/ops"]`, `trade = ["ploy-runner-host/run-full"]`, and explicit `[[bin]]` entries: `ploy-collector` requires `ops`, while `ploy-trade-runner` requires `trade`. Keep the existing combined main binary/`full` feature for development compatibility, but production packaging never uses it. `ploy-collector` recognizes only `check-db` and reviewed collector subcommands; `ploy-trade-runner` recognizes only strategy execution. Each rejects the opposite role's commands and unknown commands with non-zero `ExitCode` before runtime startup.
- The Task 1 trade manifest maps source binary name `ploy-trade-runner` to archive destination `bin/ploy-runner` and records that mapping in `release.json.binary_sources`. The packager rejects `target/.../ploy-runner` as a source even if it is a valid ELF; no later task may infer the compatibility alias from a filename glob.
- The initial research manifest requires the ELF `ploy-collector` and rejects `ployd`, `ployctl`, `ploytui`, `ploy-runner`, every strategy/deployment manifest, and every live/trade script. The initial trade manifest requires the four execution binaries and five exact service/timer capability slots listed in the role contract. Tasks 3-6 explicitly extend manifests/tests for the PostgreSQL exporter, Sidecar, activation, verifier, and live-promotion validator as those sources land.
- Fail when an allowlisted source is missing or an unallowlisted capability appears.
- Validate TOML/JSON with Python `tomllib`/`json`.
- Research rejects `runtime.mode=live`, `runtime_mode=live`, live filename patterns, private-key variable assignments, and wallet material.
- Trade rejects collectors, research binaries, `migrations/`, `sqlx`, and DB migration scripts.
- Trade requires the live manifest `desired_state=paused` and a live cap of exactly `5.00`; the account sentinel is permitted only as an unrendered template.
- Scan text files for non-empty private keys, password-bearing PostgreSQL URLs, `ALIYUN_OSS_ACCESS_KEY_SECRET`, and cloud access-key assignments.
- Sort members and normalize uid/gid/uname/gname/mode/mtime. Use Python `tarfile` plus `gzip.GzipFile(mtime=source_date_epoch)` so output is reproducible on macOS and Linux.
- Write checksums from bytes actually placed in the archive.
- Add no package/template dependency.

Step 1: Write failing shell tests.

```text
research_bundle_has_only_research_allowlist
research_bundle_has_ops_only_collector_and_no_execution_control_plane
trade_runner_has_execution_only_and_rejects_every_collector_command
trade_alias_source_is_exact_ploy_trade_runner_and_rejects_combined_binary
research_bundle_contains_only_the_exact_service_and_timer_allowlist
trade_bundle_has_only_trade_allowlist
trade_bundle_contains_all_five_exact_service_and_timer_units
same_inputs_produce_same_archive_hash
missing_binary_fails
research_live_asset_fails
trade_research_asset_fails
secret_bearing_text_file_fails_without_printing_secret
live_manifest_not_paused_fails
release_manifest_has_sorted_file_checksums
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-runner-host --no-default-features --features ops ploy_collector_rejects_run_command
bash scripts/tests/test_package_deploy_bundle.sh
```

Expected RED result: packager does not exist.

Step 3: Implement the standard-library packager.

- Create `apps/new-ploy-runner/src/bin/` before adding the dedicated binary source.
- Test fixtures create fake ELF headers (`\x7fELF`) and config trees in a temporary directory; do not build Rust in the shell test.
- Error messages name the file/rule but never echo the matched secret value.

Step 4: Verify.

```bash
rtk cargo check --locked -p new-ploy-runner --no-default-features --features ops --bin ploy-collector
rtk cargo check --locked -p new-ploy-runner --no-default-features --features trade --bin ploy-trade-runner
rtk cargo test -p ploy-runner-host --no-default-features --features ops ploy_collector_rejects_run_command
rtk cargo test -p ploy-runner-host --no-default-features --features run-full trade_runner_rejects_collector_commands
bash scripts/tests/test_package_deploy_bundle.sh
python3 -m py_compile scripts/ci/package_deploy_bundle.py
rtk git diff --check
```

Step 5: Commit.

```bash
git add scripts/ci/package_deploy_bundle.py \
  scripts/tests/test_package_deploy_bundle.sh \
  deployment/bundles/research.json \
  deployment/bundles/trade.json \
  apps/new-ploy-runner/Cargo.toml \
  apps/new-ploy-runner/src/bin/ploy-collector.rs \
  apps/new-ploy-runner/src/bin/ploy-trade-runner.rs \
  crates/ploy-runner-host/src/lib.rs
git commit -m "feat(release): package explicit research and trade bundles"
```

---

### Task 2: Narrow Tango deployment to the research/data role

Files:

- Modify `.github/workflows/deploy-tango-1-1.yml`.
- Modify `scripts/ci/deploy_tango_cloud_assist.py`.
- Add `deployment/systemd/ploy-research-orchestrator.service`.
- Modify `deployment/bundles/research.json` and `scripts/tests/test_package_deploy_bundle.sh`.
- Modify `tests/test_persist_research_trace_contract.py`.
- Add the allocated research-role tests with this implementation; do not weaken the Task 1 archive tests.

Workflow changes:

- Build/package `ploy-collector`, the PostgreSQL-to-portable-capable `research-snapshot-compile`, and a CI-built `sqlx` binary alongside existing research binaries; do not build/package `ployd`, `ployctl`, or a normal strategy runner for Tango.
- Run `npm ci`, contracts/tests/build for `ploy-sidecar` in CI. Package compiled `ploy-sidecar/dist`, `package.json`, `package-lock.json`, and lockfile-verified production dependencies (`pg` and official `@openai/codex`) via `npm ci --omit=dev`; do not package a static strategy profile. The bundle must contain executable `sidecar/node_modules/.bin/codex` with version equal to the locked package. Require host `/usr/bin/node` major version 22+ and `gh` during role verification rather than installing them in the deploy script.
- Package/install `ploy-research-orchestrator.service` with `ExecStart=/usr/bin/node /opt/ploy/current/sidecar/dist/index.js`, mandatory `EnvironmentFile=/opt/ploy/env/research-agent.env`, then fixed `SIDECAR_RUNTIME_PROFILE=polymarket_research_only`, `SIDECAR_AGENT_ENGINE=codex`, `SIDECAR_RUN_REQUEST_STORE=postgres`, `SIDECAR_CODEX_COMMAND=/opt/ploy/current/sidecar/node_modules/.bin/codex`, and `SIDECAR_GH_REPO=proerror77/ploy`. Use a dedicated unprivileged user, read-only release paths, and writable action-journal paths outside `current`. It receives a least-privilege research-queue DB URL plus model/GitHub credentials, but no wallet, trade token, daemon admin token, or cloud access key.
- Invoke `package_deploy_bundle.py --role research`.
- Remove live strategy config, live deployment manifest, and live paused-check steps from Tango.
- Retain collector freshness, Research OS, PostgreSQL portable export, event-root producer, trace planner/writer, Sidecar orchestrator, and migration assets. Remove paper/dry-run daemon health checks from the research-host postflight.
- Verify the installed bundle role from `release.json` before any host mutation.
- Require `TANGO_1_1_INSTANCE_ID`, host, SSH key, and known-host fingerprint from the protected environment; provide no instance-ID default.
- Do not fall back to `ssh-keyscan`.
- Research postflight validates Node 22+, the release-pinned Codex version/no-tool probe, `gh repo view proerror77/ploy`, active `ploy-research-orchestrator.service`, PostgreSQL queue connectivity, and that its environment contains no forbidden wallet/trade/cloud secret names. Every `gh` call has the fixed repo and works without `.git`. The child Codex process still receives only the smaller sanitized environment defined by the Agent plan.

Cloud Assistant rule:

- The uploaded remote script must not contain `ALIYUN_OSS_ACCESS_KEY_ID` or `ALIYUN_OSS_ACCESS_KEY_SECRET` values.
- GitHub runner credentials may upload the immutable bundle/script, but ECS downloads through its RAM role or the workflow stops with a clear missing-role precondition.
- The generated payload contains artifact URL/key and checksum only.

Step 1: Run the existing RED boundary tests.

```bash
rtk cargo test --locked -p ploy --test workflow_security research_bundle_rejects_all_live_assets -- --exact
rtk cargo test --locked -p ploy --test workflow_security cloud_assistant_payload_contains_no_oss_secret_value -- --exact
rtk pytest tests/test_persist_research_trace_contract.py
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
```

Step 2: Implement the narrowed workflow.

- Preserve main provenance and pinned SSH behavior from existing hardening.
- Do not dispatch the workflow.
- Do not rename Tango references in research runbooks yet; the role name remains stable.

Step 3: Verify syntax and contracts.

```bash
rtk cargo test --locked -p ploy --test workflow_security tango -- --nocapture
rtk pytest tests/test_persist_research_trace_contract.py tests/test_market_data_gap_audit_scope.py
python3 -m py_compile scripts/ci/deploy_tango_cloud_assist.py
ruby -e 'require "yaml"; YAML.load_file(ARGV.fetch(0))' .github/workflows/deploy-tango-1-1.yml
rtk git diff --check
```

Step 4: Commit.

```bash
git add .github/workflows/deploy-tango-1-1.yml \
  scripts/ci/deploy_tango_cloud_assist.py \
  deployment/systemd/ploy-research-orchestrator.service \
  deployment/bundles/research.json \
  scripts/tests/test_package_deploy_bundle.sh \
  tests/test_persist_research_trace_contract.py \
  tests/workflow_security.rs
git commit -m "ci(research): isolate Tango research deployment"
```

---

### Task 3: Make collector units use protected external database configuration

Files:

- Modify these research/data services explicitly:
  - `deployment/systemd/ploy-binance-aggtrade-collector.service`
  - `deployment/systemd/ploy-binance-lob-collector.service`
  - `deployment/systemd/ploy-binance-price-collector.service`
  - `deployment/systemd/ploy-deribit-greeks-collector.service`
  - `deployment/systemd/ploy-deribit-iv-collector.service`
  - `deployment/systemd/ploy-market-discovery.service`
  - `deployment/systemd/ploy-orderbook-snapshot-archive.service`
  - `deployment/systemd/ploy-orderbook-snapshot-retention.service`
  - `deployment/systemd/ploy-pm-trade-collector.service`
  - `deployment/systemd/ploy-polymarket-v2-indexer-import.service`
- Leave `deployment/systemd/polymarket-v2-indexer.service` on its existing standalone indexer environment contract; it has no Ploy database URL and is not a Ploy collector unit.
- Add `deployment/systemd/ploy-quote-collector.service`.
- Modify `deployment/bundles/research.json` and `scripts/tests/test_package_deploy_bundle.sh` to add the quote unit only after it exists.
- Modify `scripts/ploy_maintenance.sh`.
- Modify `tests/test_runtime_market_data_boundary.py`.
- Add `deployment/env.research.example` in Task 6, not in this commit.

Unit rules:

```ini
[Unit]
Wants=network-online.target
After=network-online.target

[Service]
User=ploy
Group=ploy
EnvironmentFile=/opt/ploy/env/research.env
```

- Remove dependency on local `postgresql.service`.
- Remove every localhost/default PostgreSQL URL and `PGPASSWORD=postgres`.
- Pass `${PLOY_DATABASE__URL}` to commands that require `--db-url`.
- Keep existing `Restart=always`, `RestartSec=5`, `MemoryHigh=1280M`, `MemoryMax=1536M`, and `OOMPolicy=kill` where the unit is a long-lived service.
- Every allowlisted research unit that invokes a Ploy command uses `/opt/ploy/current/bin/ploy-collector`; none may reference `ploy-runner`, `ployd`, or `ployctl`. The quote unit invokes `ploy-collector collect-quotes` and reuses the existing quote hardening drop-in. Archive/retention units may invoke their existing reviewed scripts but never a trade binary.
- `ploy_maintenance.sh` prefers `PLOY_DATABASE__URL`, then the existing compatibility `DATABASE_URL`.

Step 1: Add failing tests.

```text
test_collectors_require_research_environment_file
test_collectors_do_not_depend_on_local_postgresql
test_collector_database_urls_have_no_local_default
test_quote_collector_is_packaged_for_research_only
test_research_units_never_reference_ploy_runner_or_daemon_binaries
```

Step 2: Run RED.

```bash
rtk pytest tests/test_runtime_market_data_boundary.py
```

Expected RED result: units use local/default credentials and the quote service is absent.

Step 3: Update units and maintenance script.

Step 4: Verify.

```bash
rtk pytest tests/test_runtime_market_data_boundary.py
if rg -n 'postgresql://postgres:postgres|PGPASSWORD=postgres|After=.*postgresql|Wants=.*postgresql' deployment/systemd; then
  echo "local/default PostgreSQL dependency remains" >&2
  exit 1
fi
bash -n scripts/ploy_maintenance.sh
if command -v systemd-analyze >/dev/null 2>&1; then
  systemd-analyze verify deployment/systemd/*.service
else
  echo "systemd-analyze unavailable locally; Ubuntu CI contract test remains required"
fi
rtk git diff --check
```

Expected result: the credential/dependency `rg` has no matches.

Step 5: Commit.

```bash
git add deployment/systemd \
  deployment/bundles/research.json \
  scripts/tests/test_package_deploy_bundle.sh \
  scripts/ploy_maintenance.sh \
  tests/test_runtime_market_data_boundary.py
git commit -m "fix(research): externalize collector database configuration"
```

---

### Task 4: Make `release-aliyun.yml` the only complete trade release

Files:

- Add `.github/workflows/release-aliyun.yml` by moving/hardening the active logic from `release-platform.yml`.
- Add `.github/workflows/live-canary-gate.yml` as the only protected caller of the canonical live-promotion API.
- Add `.github/workflows/promote-live-config.yml` as the only parity-to-live-config PR producer.
- Modify `.github/workflows/runtime-recording-export.yml` and `.github/workflows/trade-runtime-evidence-snapshot.yml` from the Research Orchestrator prerequisite to bind their fixed read-only host environments.
- Modify `.github/workflows/runtime-candidate-replay.yml` and `.github/workflows/recorded-replay-parity.yml` to complete the trade-host/hosted-CI immutable evidence path and remove their legacy Tango execution path.
- Delete `.github/workflows/release-platform.yml`.
- Delete `.github/workflows/deploy-trade.yml`.
- Modify `scripts/install-platform-service.sh`.
- Modify `deployment/ployd.service`.
- Modify `apps/new-ployd/src/main.rs` and its focused tests.
- Modify `scripts/ploy_platform_watchdog.sh` and `scripts/tests/test_ploy_platform_watchdog.sh`.
- Modify `deployment/bundles/trade.json` and `scripts/tests/test_package_deploy_bundle.sh`.
- Modify `AGENTS.md` and `CLAUDE.md` so their Deployment and Required-policy sections name the same production workflow.
- Modify `tests/platform_release_workflow.rs`.
- Modify `tests/workflow_security.rs`.
- Modify active references in `README.md`, `docs/CONTRIBUTING.md`, `docs/DRY_RUN_PLATFORM_CHECKLIST.md`, `docs/agent-workflow.md`, `docs/operations/data-jobs-inventory.md`, `docs/runbooks/live-deployment-checklist.md`, `docs/runbooks/platform-deploy.md`, `docs/runbooks/secrets-rotation.md`, and `docs/runbooks/strategy-research-cicd.md`. Do not rewrite archived/historical design or review documents.

Trade workflow rules:

- `release-aliyun.yml` is the single production-named workflow. `release-platform.yml` and `deploy-trade.yml` are deleted, and all active docs/tests/instructions are updated; archived history may retain names.
- Protected environment is `ploy-trade-1`.
- Host/key/known-host inputs use the existing `PLOY_TRADE_1_*` protected values.
- Deploy/install mode requires `git_ref=main`, checked-out SHA equal to `origin/main`, and a successful required Test workflow whose `headSha` exactly equals the release SHA.
- Build the complete Linux release binary allowlist in CI and verify each binary begins with ELF magic.
- Build `ploy-trade-runner` separately with `new-ploy-runner/trade` and no default/ops feature, then package it as `bin/ploy-runner`; contract tests invoke every collector/check-db subcommand and require non-zero before packaging.
- Invoke `package_deploy_bundle.py --role trade` and verify checksum before upload/install.
- Trade install never invokes `sqlx`, `psql`, migrations, Cargo, or rustc.
- Bundle includes the live gate, paper drill, exact live config, paused live manifest template, core service, maintenance, and watchdog.
- Extend the trade manifest only now that `validate_live_promotion_gate.py` and the hardened gate exist. Package both and reject a bundle containing an older warning/skip-capable gate.
- Bundle contains exactly `ployd.service`, `ploy-maintenance.service`, `ploy-maintenance.timer`, `ploy-platform-watchdog.service`, and `ploy-platform-watchdog.timer`; enable both timers during install and assert them active in postflight.
- Delete the incomplete runner/config-only `deploy-trade.yml`; one role has one deploy owner.
- Keep the remote install job explicitly disabled behind a repository-owned `DEPLOYMENT_CONTRACT_READY=false` constant through Tasks 4-8. Feature-branch/build-only runs can build and validate archives, but no input can override that constant. Task 9 changes it to true only after the entire acceptance matrix passes.

Replay/parity host migration:

- Keep repository-owned `DUAL_HOST_EVIDENCE_READY=false` in all four evidence workflows through Tasks 4-8. Task 9 is the only task permitted to flip it, together with `DEPLOYMENT_CONTRACT_READY`, after both role and evidence-chain tests pass. A dispatch input, environment value, or secret cannot override either constant.
- Bind `runtime-recording-export.yml` only to `tango-1-1-evidence` and `TANGO_1_1_*` read-only SSH/known-host values. Bind `trade-runtime-evidence-snapshot.yml` and `runtime-candidate-replay.yml` only to `ploy-trade-1-evidence` and `PLOY_TRADE_1_HOST`/SSH/known-host/read-only-report values. `recorded-replay-parity.yml` is hosted-CI-only and receives no host secret.
- `scripts/install-platform-service.sh` creates the unprivileged `ploy-evidence` account/directory contract without a login shell, release/config write permission, service-control privilege, wallet environment, daemon Admin token, live-gate HMAC key, database URL, or cloud credential. The evidence workflow may execute only the release-pinned runner in replay mode inside its mode-0700 temporary directory and must remove it on every exit.
- Use the exact immutable chain from the Agent plan: Event ML candidate config artifact + research recording artifact + `/opt/ploy/releases/<sha>/release.json` and runner hashes -> candidate replay artifact; then candidate replay artifact + trade dry-run config/report/release artifact -> `RecordedRuntimeParityV2`. Every resolver pins repository, workflow, run ID, artifact ID/name, action ID, main head SHA, horizon, time window, and recomputed content SHA-256. No `latest`, caller path/hash, or mutable endpoint fallback exists.
- Tests parse complete workflow YAML/run blocks and require `runtime-candidate-replay.yml` and `recorded-replay-parity.yml` to contain no `TANGO_1_1`, `tango-1-1-build-only`, Tango recording path, or Tango SSH reference. Only `runtime-recording-export.yml` may bridge the research host; a hash/source/window/release mismatch blocks before replay/parity.

Protected live-canary workflow:

- `live-canary-gate.yml` is `workflow_dispatch` only, shares the non-overridable `DEPLOYMENT_CONTRACT_READY=false` hard stop until Task 9, requires exact `git_ref=main`, environment `ploy-trade-1-live` with human reviewers, and inputs limited to recorded-parity run/artifact ID, RDS recovery-proof run/artifact ID, approval ID, emergency/restart audit IDs, and the exact USD 5 acknowledgement. It has no strategy/config/model/path/free-form command input.
- Resolve the post-config-merge parity run by exact workflow `recorded-replay-parity.yml`, successful conclusion, `workflow_dispatch`, repository, current `origin/main` SHA, run ID, action/provenance ID, and artifact name. Download it through GitHub API, validate `RecordedRuntimeParityV2` against the generated schema, recompute artifact SHA-256, and require `live_config_materialized=true` plus every strict field/metric/blocker condition. Require the current checked-out fixed live config and deployed release file to hash exactly to `expected_live_config_sha256`, and runner SHA to equal current main. A caller-supplied local JSON, config, or hash is never accepted.
- Resolve an independent successful protected `verify-rds-pitr-restore.yml` run on the same main SHA. Validate retained `rds-recovery-proof.v1`, proof age at most seven days, exact database/migration lineage through 051, PITR restore/check success, and artifact SHA. Include that reference/hash in the HMAC-signed request and human approval; missing/stale/manual backup text blocks live.
- Build `HumanLiveApproval` from the protected reviewer/actor and bounded timestamps, combine it with parity plus exact release SHA/emergency-recovery audit IDs, and sign canonical `ProtectedWorkflowProvenance` using environment secret `PLOY_LIVE_GATE_HMAC_KEY`. Mask the signature/key and never upload the signed request as a public artifact.
- Transfer only the mode-0600 request to the verified trade host, invoke `pm5d_threelayer_live_gate.sh --go-live --request <path>`, and delete it on every exit path. The script calls only `ployctl system approve-live-canary`; generic live resume is forbidden.
- The Research Agent action allowlist, Sidecar environment, and research workflows do not include this workflow name, environment, HMAC key, host credential, or approval action. No automation can satisfy the required GitHub environment review.

Audited live-config promotion:

- `promote-live-config.yml` is `workflow_dispatch` only under human-reviewed environment `ploy-live-config-promotion`. Inputs are exact recorded-parity run/artifact IDs and acknowledgement `PREPARE_REVIEWABLE_LIVE_CONFIG_PR`; there is no config path, content, merge, deploy, or live input.
- Require current `origin/main` to equal the pre-promotion parity's `source_head_sha`, verify that exact successful-main provenance, download its immutable live-config candidate, rerun `build_live_config_candidate.py` from the checked-out dry-run config, and require bytes/hashes equal `expected_live_config_sha256`.
- Select source/destination from a hard-coded horizon map. Change only the generated PM5D/PM15D live config and a machine-readable promotion receipt; manifest remains unrendered/paused. Open a review-required PR and stop. Never auto-approve, auto-merge, release, deploy, render a wallet, or call a host.
- After the PR merges, recorded replay/dry-run parity must run again on the new exact main/release SHA. `live-canary-gate.yml` accepts only this later artifact with `live_config_materialized=true`, current/deployed live bytes equal to the expected hash, and runner SHA equal to current main. Thus the config-only merge does not create a stale-runner shortcut and an untracked manual edit cannot reach live.

Runtime environment and watchdog boundary:

- `ployd.service` uses mandatory `EnvironmentFile=/opt/ploy/env/platform-live.env` (no leading `-`), `WorkingDirectory=/opt/ploy/current`, and fixed `Environment=PLOY_DOTENV_MODE=disabled` after the environment-file directive. The bundle contains no `.env`.
- Replace unconditional `dotenvy::dotenv()` in `new-ployd` with explicit modes: production `disabled`, opt-in local-development `local_optional`, and an explicit required path for tests/tools. Invalid mode or missing required file fails before daemon boot. Systemd's fixed disabled mode cannot be overridden by a value inside the environment file.
- `scripts/ploy_platform_watchdog.sh` defaults to exactly `ployd.service`; a missing unit is an error on the trade role, not a successful skip. An explicit test override remains possible. Tests cover active, inactive/start, maintenance lock, stop lock, and missing-unit failure using `ployd.service`, not legacy `ploy-platform.service`.
- The trade role verifier later checks the exact unit/env/dotenv/watchdog contract before start, so the file it validates is the file the process actually consumes.

Postflight:

```text
systemctl is-active ployd
GET /health
ployctl system status
ployctl system metrics
ployctl system alerts
ployctl trading status
GET /api/deployments reports zero canonical live deployments on a fresh host
on-disk pm5d live template has desired=Paused and the unrendered account sentinel
systemctl is-active ploy-maintenance.timer ploy-platform-watchdog.timer
no active cargo or rustc process
ployd environment source is exactly /opt/ploy/env/platform-live.env and dotenv fallback is disabled
watchdog target is exactly ployd.service
```

The live manifest template is rendered/applied only by the future protected live gate after a normalized account ID is supplied. Only that later gate may assert canonical `pm5d.threelayer.live desired=Paused observed=Paused`; release install neither renders nor applies the sentinel template.

Step 1: Run RED tests.

```bash
rtk cargo test --locked -p ploy --test platform_release_workflow
rtk cargo test --locked -p ploy --test workflow_security trade_release_requires_same_sha_green_test -- --exact
rtk cargo test --locked -p ploy --test workflow_security remote_install_contains_no_cargo_or_rustc -- --exact
rtk cargo test --locked -p ploy --test workflow_security live_canary_requires_human_environment_exact_main_parity_and_hmac -- --exact
rtk cargo test --locked -p ploy --test workflow_security live_config_promotion_is_exact_parity_review_only_pr -- --exact
rtk cargo test --locked -p ploy --test workflow_security replay_and_parity_use_immutable_dual_host_evidence_without_tango_execution -- --exact
rtk cargo test -p new-ployd dotenv --lib
bash scripts/tests/test_ploy_platform_watchdog.sh
```

Step 2: Implement and delete the second trade owner.

- Keep build-only workflow mode safe for feature branches.
- Do not dispatch the workflow.

Step 3: Verify references and syntax.

```bash
rtk cargo test --locked -p ploy --test platform_release_workflow
rtk cargo test --locked -p ploy --test workflow_security host_deploy_workflows_require_main_provenance_and_pinned_ssh -- --exact
if rg -n 'deploy-trade\.yml|release-platform\.yml' README.md docs .github tests AGENTS.md CLAUDE.md \
  --glob '!docs/archive/**' --glob '!docs/plans/**' --glob '!docs/reviews/**' \
  --glob '!docs/superpowers/plans/**'; then
  echo "active deploy-trade.yml reference remains" >&2
  exit 1
fi
ruby -e 'require "yaml"; ARGV.each { |p| YAML.load_file(p) }' \
  .github/workflows/release-aliyun.yml \
  .github/workflows/live-canary-gate.yml \
  .github/workflows/promote-live-config.yml \
  .github/workflows/runtime-recording-export.yml \
  .github/workflows/trade-runtime-evidence-snapshot.yml \
  .github/workflows/runtime-candidate-replay.yml \
  .github/workflows/recorded-replay-parity.yml
rtk git diff --check
```

Expected result: active legacy trade-workflow references have no matches; archived history may retain their names.

Step 4: Commit.

```bash
git add .github/workflows/release-aliyun.yml \
  .github/workflows/live-canary-gate.yml \
  .github/workflows/promote-live-config.yml \
  .github/workflows/runtime-recording-export.yml \
  .github/workflows/trade-runtime-evidence-snapshot.yml \
  .github/workflows/runtime-candidate-replay.yml \
  .github/workflows/recorded-replay-parity.yml \
  .github/workflows/release-platform.yml \
  .github/workflows/deploy-trade.yml \
  scripts/install-platform-service.sh \
  deployment/ployd.service \
  apps/new-ployd/src/main.rs \
  scripts/ploy_platform_watchdog.sh \
  scripts/tests/test_ploy_platform_watchdog.sh \
  deployment/bundles/trade.json \
  scripts/tests/test_package_deploy_bundle.sh \
  AGENTS.md CLAUDE.md \
  tests/platform_release_workflow.rs \
  tests/workflow_security.rs \
  README.md \
  docs/CONTRIBUTING.md \
  docs/DRY_RUN_PLATFORM_CHECKLIST.md \
  docs/agent-workflow.md \
  docs/operations/data-jobs-inventory.md \
  docs/runbooks/live-deployment-checklist.md \
  docs/runbooks/platform-deploy.md \
  docs/runbooks/secrets-rotation.md \
  docs/runbooks/strategy-research-cicd.md
git commit -m "ci(trade): make Aliyun release the sole deploy owner"
```

---

### Task 5: Add atomic SHA activation and rollback

Files:

- Add `scripts/activate_release.sh`.
- Add `scripts/tests/test_activate_release.sh`.
- Modify both `deployment/bundles/*.json` manifests and `scripts/tests/test_package_deploy_bundle.sh` to add the activator only after it exists.
- Modify `scripts/install-platform-service.sh`.
- Modify `deployment/ployd.service`.
- Modify `deployment/ploy-maintenance.service`.
- Modify `deployment/ploy-platform-watchdog.service`.
- Modify every research service in the Task 3 allowlist so executable/script paths resolve through `/opt/ploy/current`.
- Modify `.github/workflows/release-aliyun.yml`.
- Modify `.github/workflows/deploy-tango-1-1.yml`.
- Modify `docs/runbooks/rollback.md`.

Directory contract:

```text
/opt/ploy/releases/<sha>/
/opt/ploy/current  -> releases/<sha>
/opt/ploy/previous -> releases/<previous-sha>
```

CLI:

```text
scripts/activate_release.sh --root <root> --role research|trade --release <release-dir>
scripts/activate_release.sh --root <root> --role research|trade --rollback
```

Activation rules:

- Verify release manifest schema, role, 40-character SHA, every file checksum, and expected release directory name.
- Refuse a dirty/incomplete release directory.
- Atomically update `previous`, then `current`, using temporary symlinks plus `mv` in the same filesystem.
- `--rollback` verifies `previous` before swapping.
- Activation script does not restart systemd or call HTTP. Workflow owns restart/postflight.
- After every normal symlink activation, install the role's exact unit allowlist from the new `current`, remove any previously installed Ploy unit outside that allowlist, run `systemctl daemon-reload`, then restart/enable and postflight.
- On postflight failure, workflow invokes `--rollback`, then repeats that same unit reconciliation from the restored `current` before `daemon-reload`, restart, and the identical postflight. A symlink swap alone is never considered unit rollback. If rollback unit install or postflight fails, stop and surface both failures.
- Migrations must remain backward-compatible across the rollback window; an irreversible migration is a separate no-go decision.
- Both research and trade workflows unpack to `releases/<sha>`, call the same role-aware activator, reconcile/install their exact unit allowlist from `current` on both forward and rollback paths, and use the same rollback/postflight sequence. Neither workflow copies binaries or scripts into fixed `/opt/ploy/bin` or `/opt/ploy/scripts` paths.

Service paths:

```ini
ExecStart=/opt/ploy/current/bin/ployd
EnvironmentFile=/opt/ploy/env/platform-live.env
Environment=PLOY_RUNNER_BINARY=/opt/ploy/current/bin/ploy-runner
Environment=PLOY_STRATEGY_CONFIG_ROOT=/opt/ploy/current/config/strategies
```

All research collector services use `/opt/ploy/current/bin/ploy-collector`; archive/retention services and trade maintenance/watchdog use their explicitly packaged script under `/opt/ploy/current/scripts`. Stable writable data, log, run, and protected env paths remain outside the release symlink.

The trade environment file is mandatory (no leading `-`). The installer creates directories/permissions but never fabricates auth, database, wallet, or account defaults; the protected workflow materializes `/opt/ploy/env/platform-live.env`, runs the role verifier against that exact path, then starts systemd.

The trade `ploy-maintenance.service` sets `PLOY_MAINTENANCE_LOGS_ONLY=true`. It performs host log/tmp/recording cleanup only and never invokes `psql`, `runuser postgres`, or local-Postgres fallback; research data retention remains on the research role's dedicated units/workflows.

Step 1: Add failing shell tests.

```text
first_activation_sets_current_without_previous
second_activation_sets_previous_and_current
rollback_swaps_to_verified_previous
role_or_sha_mismatch_fails_without_changing_symlinks
checksum_failure_fails_without_changing_symlinks
failed_postflight_path_can_restore_previous_release
rollback_reinstalls_previous_unit_definitions_before_restart
rollback_removes_units_not_allowlisted_by_previous_release
research_and_trade_workflows_activate_and_rollback_the_same_sha_contract
all_packaged_unit_executables_resolve_through_current_symlink
trade_service_requires_verified_nonoptional_environment_file
trade_maintenance_logs_only_succeeds_without_database_or_postgres_user
```

Step 2: Run RED.

```bash
bash scripts/tests/test_activate_release.sh
```

Expected RED result: activation script does not exist and current installer overwrites fixed paths.

Step 3: Implement the narrow script and workflow integration.

Step 4: Verify.

```bash
bash scripts/tests/test_activate_release.sh
bash -n scripts/activate_release.sh scripts/install-platform-service.sh
rtk pytest tests/test_runtime_market_data_boundary.py
rtk cargo test --locked -p ploy --test platform_release_workflow
rtk git diff --check
```

Step 5: Commit.

```bash
git add scripts/activate_release.sh \
  scripts/tests/test_activate_release.sh \
  deployment/bundles/research.json \
  deployment/bundles/trade.json \
  scripts/tests/test_package_deploy_bundle.sh \
  scripts/install-platform-service.sh \
  deployment/ployd.service \
  deployment/ploy-maintenance.service \
  deployment/ploy-platform-watchdog.service \
  deployment/systemd \
  .github/workflows/release-aliyun.yml \
  .github/workflows/deploy-tango-1-1.yml \
  docs/runbooks/rollback.md
git commit -m "feat(release): activate and roll back immutable SHA releases"
```

---

### Task 6: Add role and secret fail-closed verification

Files:

- Add `scripts/verify_deploy_role.sh`.
- Add `scripts/tests/test_verify_deploy_role.sh`.
- Add `deployment/env.research.example`.
- Add `deployment/env.research-agent.example`.
- Modify `deployment/env.platform-live.example`.
- Modify `deployment/ployd.service`, `deployment/systemd/ploy-research-orchestrator.service`, both `deployment/bundles/*.json`, and `scripts/tests/test_package_deploy_bundle.sh`.
- Modify `docs/runbooks/secrets-rotation.md`.
- Modify research/trade workflows to call the verifier before service start.

CLI:

```text
scripts/verify_deploy_role.sh research <release-root> <research-env-file> <research-agent-env-file>
scripts/verify_deploy_role.sh trade <release-root> <env-file>
```

Research rules:

- Require protected database URL plus research trace/operator configuration; no daemon auth is required because the research bundle has no `ployd`.
- Require TLS mode `require`, `verify-ca`, or `verify-full` in the database URL/config.
- Reject any non-empty Polymarket private key, relayer signer material, `PLOY_WORKER_TOKEN`, live config, live manifest, live runtime mode, `ployd`, `ployctl`, or normal strategy runner.
- Require the separate Agent env file to contain `SIDECAR_CODEX_API_KEY`, repository-scoped `GH_TOKEN`, a fixed repo matching `proerror77/ploy`, `SIDECAR_RUN_REQUEST_STORE=postgres`, a TLS `SIDECAR_RESEARCH_QUEUE_DATABASE_URL` for the queue-only database role, and explicit research action gates. The systemd-fixed profile/engine/Codex command must equal the research-only contract and cannot be overridden by the file. Reject the broad `PLOY_DATABASE__URL`, cloud credentials, wallet/trade/daemon tokens, live approval material, and deployment gates there. Verify Node 22+, the release-pinned Codex CLI/no-tool probe, and a fixed-repo `gh repo view` without printing auth status.
- Verify the Sidecar unit consumes exactly `/opt/ploy/env/research-agent.env`, while collector units consume exactly `/opt/ploy/env/research.env`; no unit may consume both.

Trade rules:

- Require distinct daemon admin/operator/worker tokens, normalized live account ID, private key/signature/funder configuration, and positive cap.
- Require a distinct high-entropy `PLOY_LIVE_GATE_HMAC_KEY`; it may be materialized only by the protected trade environment and must match the masked secret used by `live-canary-gate.yml`. Reject equality with any daemon token or wallet key.
- Require `PLOY_CANONICAL_STORE=postgres` and a protected `PLOY_DATABASE__URL` for the canonical trading/audit store with TLS mode `require`, `verify-ca`, or `verify-full`; file mode, localhost URL, plaintext mode, embedded default credential, or missing URL blocks trade service start.
- Reject equal admin/operator/worker token values so a strategy worker cannot authenticate as operator/admin.
- Require the live manifest to remain paused before and after rendering.
- Reject research binaries, collectors, migrations, and automatic migration flags.
- Verify `ployd.service` consumes exactly mandatory `/opt/ploy/env/platform-live.env`, fixes `PLOY_DOTENV_MODE=disabled`, uses `/opt/ploy/current`, and the release contains no `.env`. Verify watchdog defaults to `ployd.service`; a verifier success against any other env/unit contract is impossible.
- Never print secret values; output only missing/invalid variable names.

Example env files:

- Contain variable names and safe comments, not localhost/default credentials or realistic key values.
- Research example has no wallet fields.
- Research Agent example has only the queue-scoped TLS database field and no broad research/trading database, wallet, cloud, daemon, or live-approval fields.
- Trade example has no automatic migration field.

Step 1: Add failing shell tests.

```text
research_rejects_wallet_key_and_live_asset
research_rejects_execution_control_plane_binaries_and_worker_token
research_requires_tls_database_url
research_agent_requires_node_gh_dedicated_key_and_separate_env
research_agent_requires_pinned_codex_fixed_repo_and_polymarket_only_profile
research_units_do_not_cross_load_collector_and_agent_env
trade_requires_wallet_signature_and_paused_manifest
trade_requires_distinct_admin_operator_and_worker_tokens
trade_requires_distinct_live_gate_hmac_key_without_printing_it
trade_requires_tls_database_url
trade_rejects_migrations_and_research_binaries
trade_verifies_exact_platform_live_env_and_disables_dotenv_fallback
errors_never_echo_secret_values
example_env_files_have_no_usable_default_credentials
```

Step 2: Run RED.

```bash
bash scripts/tests/test_verify_deploy_role.sh
```

Step 3: Implement and wire build-only validation.

Step 4: Verify.

```bash
bash scripts/tests/test_verify_deploy_role.sh
bash -n scripts/verify_deploy_role.sh
if rg -n 'POLYMARKET_PRIVATE_KEY=0x|postgres(ql)?://[^:]+:[^@]+@' deployment dist 2>/dev/null; then
  echo "usable credential remains in deploy assets" >&2
  exit 1
fi
rtk cargo test --locked -p ploy --test workflow_security
rtk git diff --check
```

Expected result: the credential `rg` has no matches.

Step 5: Commit.

```bash
git add scripts/verify_deploy_role.sh \
  scripts/tests/test_verify_deploy_role.sh \
  deployment/env.research.example \
  deployment/env.research-agent.example \
  deployment/env.platform-live.example \
  deployment/ployd.service \
  deployment/systemd/ploy-research-orchestrator.service \
  deployment/bundles/research.json \
  deployment/bundles/trade.json \
  scripts/tests/test_package_deploy_bundle.sh \
  docs/runbooks/secrets-rotation.md \
  .github/workflows/deploy-tango-1-1.yml \
  .github/workflows/release-aliyun.yml
git commit -m "fix(deploy): verify role and secret boundaries"
```

---

### Task 7: Remove legacy host identity and insecure fingerprint fallback

Files:

- Modify `.github/workflows/deploy-tango-1-1.yml`.
- Modify `.github/workflows/research-snapshot.yml`.
- Modify `.github/workflows/market-data-gap-audit.yml`.
- Modify `.github/workflows/backtest.yml`.
- Modify `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml`.
- Modify `.github/workflows/research-trace-plan.yml`.
- Modify `scripts/ci/deploy_tango_cloud_assist.py`.
- Modify `scripts/ci/run_tango_market_data_audit_cloud_assist.py`.
- Modify `scripts/ci/run_tango_research_snapshot_cloud_assist.py`.
- Modify `tests/workflow_security.rs`.
- Modify `docs/runbooks/strategy-research-cicd.md`.

Rules:

- Remove every default ECS instance ID and fixed private IP.
- Require host, instance ID, and known-host values from the protected research environment.
- `backtest.yml` compares the DB URL hostname with protected `PLOY_RESEARCH_DB_HOST`; it does not compare against a hard-coded IP.
- Never auto-accept a fingerprint with `ssh-keyscan`.
- Keep `StrictHostKeyChecking yes`, `HostKeyAlias`, and explicit `UserKnownHostsFile`.

Step 1: Run the RED scan/test.

```bash
rtk cargo test --locked -p ploy --test workflow_security workflows_have_no_hard_coded_ecs_identity_or_private_ip -- --exact
rg -n 'i-6we8940uojnsory3ihfw|172\.16\.0\.204|ssh-keyscan' \
  .github scripts tests docs --glob '!docs/archive/**' --glob '!docs/superpowers/plans/**'
```

Expected RED result: active files still contain legacy identity/fallback values.

Step 2: Remove them and make missing protected values fatal.

Step 3: Verify.

```bash
rtk cargo test --locked -p ploy --test workflow_security host_deploy_workflows_require_main_provenance_and_pinned_ssh -- --exact
rtk pytest tests/test_market_data_gap_audit_scope.py tests/test_persist_research_trace_contract.py
if rg -n 'i-6we8940uojnsory3ihfw|172\.16\.0\.204|ssh-keyscan' \
  .github scripts tests docs --glob '!docs/archive/**' --glob '!docs/superpowers/plans/**'; then
  echo "legacy host identity or fingerprint fallback remains" >&2
  exit 1
fi
rtk git diff --check
```

Expected result: final `rg` has no matches.

Step 4: Commit.

```bash
git add .github/workflows/deploy-tango-1-1.yml \
  .github/workflows/research-snapshot.yml \
  .github/workflows/market-data-gap-audit.yml \
  .github/workflows/backtest.yml \
  .github/workflows/factor-walk-forward-v2-hosted-artifact.yml \
  .github/workflows/research-trace-plan.yml \
  scripts/ci/deploy_tango_cloud_assist.py \
  scripts/ci/run_tango_market_data_audit_cloud_assist.py \
  scripts/ci/run_tango_research_snapshot_cloud_assist.py \
  tests/workflow_security.rs \
  docs/runbooks/strategy-research-cicd.md
git commit -m "fix(deploy): remove legacy host identity fallbacks"
```

---

### Task 8: Govern research migrations and RDS recovery proof separately

Files:

- Add `.github/workflows/migrate-research-db.yml`.
- Add `.github/workflows/verify-rds-pitr-restore.yml`.
- Modify `.github/workflows/deploy-tango-1-1.yml`.
- Modify `tests/workflow_security.rs`.
- Add `docs/runbooks/research-database-migrations.md`.

Workflow inputs:

```yaml
git_ref:        # default main
execute:        # boolean, default false
backup_ref:     # required and non-empty for execute
execute_ack:    # exact migrate-research-db for execute
```

Recovery-proof workflow:

- `verify-rds-pitr-restore.yml` defaults to validation-only and cannot emit proof. Execute mode requires exact main/same-SHA Test, protected `ploy-research-recovery` environment with human review, source RDS identity/recovery point from protected values, exact `VERIFY_RDS_PITR_RESTORE`, and a bounded cleanup timeout.
- Use short-lived RAM/OIDC credentials to create an isolated temporary PITR restore, wait for TLS readiness, run read-only schema/migration checks through migration 051 plus bounded integrity/hash probes, and delete the temporary instance on success or failure. No trade host, wallet, or live service is contacted.
- Upload `rds-recovery-proof.v1` only after restore checks and confirmed deletion. It carries workflow/run/main SHA, source backup/recovery-point reference, restored engine/schema/migration versions, check hashes/counts, started/completed/deleted timestamps, cleanup result, and no URL/password/credential. Artifact retention must cover the seven-day live-gate freshness window.
- If cleanup cannot be proven, mark the workflow failed, emit no acceptable proof, and raise a critical manual cleanup notice. The later live-canary workflow validates the exact successful run/artifact/hash and freshness.

Rules:

- Build-only/default mode validates the research bundle's CI-built `sqlx`, migrations, provenance, and pending status contract without connecting to a database.
- Execute requires exact `origin/main` SHA, same-SHA green Test, protected `tango-1-1` environment, explicit backup/PITR reference, and exact ACK.
- Execute uses the research bundle's `sqlx migrate info`, `sqlx migrate run`, then `sqlx migrate info` against the protected TLS URL.
- The first compatible rollout must include migrations 049 (canonical runtime/audit), 050 (single-use live approvals), and 051 (cross-host research Agent queue) in order. Post-migration verification checks all tables/constraints without reading payload/secret values.
- Provisioning the Sidecar queue-only database role remains an explicit later RDS admin step. The runbook grants only the 051 queue/result tables, verifies privilege denial on runtime/audit/approval/market tables, and records the grant evidence without a credential value.
- Research deploy only verifies there is no pending migration and never mutates schema implicitly.
- Trade release has no migration credentials, binary, workflow call, or permission.
- An irreversible migration is documented as a separate no-go/maintenance decision.

Step 1: Add failing workflow tests.

```rust
fn research_migration_workflow_defaults_to_build_only()
fn research_migration_execute_requires_main_test_backup_and_ack()
fn research_deploy_does_not_run_migrations()
fn trade_release_has_no_migration_surface()
fn migration_order_contains_canonical_live_approval_and_agent_queue_tables()
fn rds_restore_proof_requires_protected_execute_and_confirmed_cleanup()
fn live_canary_requires_fresh_successful_rds_recovery_artifact()
```

Step 2: Run RED.

```bash
rtk cargo test --locked -p ploy --test workflow_security research_migration_workflow_defaults_to_build_only -- --exact
```

Step 3: Add the workflow and remove implicit migration loops from research deploy.

Step 4: Verify without dispatch.

```bash
rtk cargo test --locked -p ploy --test workflow_security research_migration -- --nocapture
ruby -e 'require "yaml"; YAML.load_file(ARGV.fetch(0)); YAML.load_file(ARGV.fetch(1))' \
  .github/workflows/migrate-research-db.yml .github/workflows/verify-rds-pitr-restore.yml
if command -v actionlint >/dev/null 2>&1; then
  actionlint .github/workflows/migrate-research-db.yml .github/workflows/verify-rds-pitr-restore.yml
else
  echo "actionlint unavailable locally; repository workflow tests and CI remain required"
fi
rtk git diff --check
```

Step 5: Commit.

```bash
git add .github/workflows/migrate-research-db.yml \
  .github/workflows/verify-rds-pitr-restore.yml \
  .github/workflows/deploy-tango-1-1.yml \
  tests/workflow_security.rs \
  docs/runbooks/research-database-migrations.md
git commit -m "ci(database): govern research migrations separately"
```

---

### Task 9: Run complete local packaging and workflow acceptance

Files:

- Add `docs/runbooks/dual-host-deployment.md`.
- Modify `README.md` deployment summary.
- Modify `.github/workflows/release-aliyun.yml`, `.github/workflows/live-canary-gate.yml`, `.github/workflows/runtime-recording-export.yml`, `.github/workflows/trade-runtime-evidence-snapshot.yml`, `.github/workflows/runtime-candidate-replay.yml`, `.github/workflows/recorded-replay-parity.yml`, and their workflow tests to flip the repository-owned deployment/evidence contract constants only after the full matrix is green.
- Modify `tasks/todo.md` with exact verification results.

Documentation must clearly separate:

- local build/test evidence;
- build-only GitHub workflow evidence;
- future research-host deployment evidence;
- future trade-host deployment evidence;
- later live canary approval.

Run locally:

```bash
bash scripts/tests/test_package_deploy_bundle.sh
bash scripts/tests/test_activate_release.sh
bash scripts/tests/test_verify_deploy_role.sh
bash scripts/tests/test_ploy_platform_watchdog.sh
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar

rtk pytest \
  tests/test_runtime_market_data_boundary.py \
  tests/test_market_data_gap_audit_scope.py \
  tests/test_persist_research_trace_contract.py \
  tests/test_polymarket_v2_indexer_contracts.py

rtk cargo test --locked -p ploy --test workflow_security
rtk cargo test --locked -p ploy --test platform_release_workflow
rtk cargo test --locked --workspace

python3 -m py_compile \
  scripts/ci/package_deploy_bundle.py \
  scripts/ci/deploy_tango_cloud_assist.py \
  scripts/ci/run_tango_market_data_audit_cloud_assist.py \
  scripts/ci/run_tango_research_snapshot_cloud_assist.py
python3 -m py_compile scripts/validate_live_promotion_gate.py
if command -v actionlint >/dev/null 2>&1; then actionlint; fi
rtk git diff --check
rtk git status --short
```

Expected result:

- both bundle tests prove mutually exclusive contents;
- rollback test restores the prior SHA in a temporary root;
- no old host identity/default credential/secret-bearing script exists;
- research cannot acquire live capability;
- trade cannot acquire research/migration capability;
- no workflow was dispatched and no remote/cloud state changed;
- only after the first complete green pass, set `DEPLOYMENT_CONTRACT_READY=true`, rerun workflow/package/role/watchdog tests, and prove no workflow input can bypass or alter that constant;
- in the same final acceptance commit set `DUAL_HOST_EVIDENCE_READY=true`, rerun the exact recording/config/release/runner/report artifact-chain tests, and prove replay/parity YAML has no Tango execution or mutable-artifact fallback;
- worktree is clean after the documentation/evidence commit.

Commit:

```bash
git add .github/workflows/release-aliyun.yml \
  .github/workflows/live-canary-gate.yml \
  .github/workflows/runtime-recording-export.yml \
  .github/workflows/trade-runtime-evidence-snapshot.yml \
  .github/workflows/runtime-candidate-replay.yml \
  .github/workflows/recorded-replay-parity.yml \
  tests/platform_release_workflow.rs \
  tests/workflow_security.rs \
  docs/runbooks/dual-host-deployment.md \
  README.md \
  tasks/todo.md
git commit -m "docs(deploy): record dual-host local readiness"
```

## Completion Criteria

- Research and trade archives are allowlist-built and mutually exclusive.
- Tango owns research/data deployment; `release-aliyun.yml` owns trade; `release-platform.yml` and `deploy-trade.yml` are gone.
- Releases install under immutable SHA directories with tested atomic rollback.
- Same-SHA green Test, ELF, checksums, paused manifests, and main provenance are enforced.
- Runtime recording, candidate replay, dry-run report, release/config/runner, and recorded parity are connected only through exact retained artifacts and recomputed hashes; replay/parity no longer execute on Tango.
- Remote install contains no Rust build or database migration command.
- Watchdog covers `ployd.service` and service guardrails remain enforced.
- The research bundle runs the no-tool Sidecar under its own mandatory agent env and contains no execution credentials.
- The trade process consumes only mandatory `platform-live.env`, cannot load a working-directory `.env`, and packages the executable parity/human-approval gate.
- Database URLs come from required protected env files and require TLS.
- Uploaded scripts contain no long-lived OSS/cloud secret.
- Legacy host IDs/IPs/fingerprint fallbacks are absent.
- Migration execution is separate, protected, backup-gated, and research-only.
- No cloud resource, remote host, or live trading state was changed locally.
